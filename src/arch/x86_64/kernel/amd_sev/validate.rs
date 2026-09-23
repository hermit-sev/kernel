use core::ops::Range;

use ghcb::instructions::pvalidate::pvalidate;
use ghcb::mapping::mapping_utils;
use ghcb::msr::GhcbMsr;
use ghcb::msr::page_state_change::{PageStateChangeRequest, PageStateOperation};
use ghcb::protocols::GhcbProtocolRequest;
use ghcb::protocols::change_page_state::{
	ChangePageStateError, ChangePageStateRequest, PageStateChangeEntry, PageStateChangeOperation,
	PageStateChangePageSize,
};
use hermit_sync::InterruptTicketMutex;
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::{PageSize, PhysFrame, Size2MiB, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use super::StaticGhcbManager;

const MAX_VALIDATABLE_ADDRESS: u64 = 512 << 30;

static VALIDATED_FRAMES: InterruptTicketMutex<ValidatedFrames> =
	InterruptTicketMutex::new(ValidatedFrames::new());

/// Validation state of the 2 MiB frames below [`MAX_VALIDATABLE_ADDRESS`].
///
/// A validated frame is private and validated.
struct ValidatedFrames([u64; (MAX_VALIDATABLE_ADDRESS / Size2MiB::SIZE / 64) as usize]);

impl ValidatedFrames {
	/// Number of entries per page state change request: GHCB's 253 entries minus the count
	/// in the headers `end_entry`.
	const MAX_PSC_ENTRIES: usize = 252;

	const fn new() -> Self {
		Self([0; _])
	}

	fn index(frame: PhysFrame<Size2MiB>) -> (usize, u64) {
		let addr = frame.start_address();
		assert!(
			addr.as_u64() < MAX_VALIDATABLE_ADDRESS,
			"physical address {addr:p} is too large for memory validation"
		);
		let n = (addr.as_u64() / Size2MiB::SIZE) as usize;
		(n / 64, 1 << (n % 64))
	}

	fn contains(&self, frame: PhysFrame<Size2MiB>) -> bool {
		let (word, bit) = Self::index(frame);
		self.0[word] & bit != 0
	}

	fn insert(&mut self, frame: PhysFrame<Size2MiB>) {
		let (word, bit) = Self::index(frame);
		self.0[word] |= bit;
	}

	fn remove(&mut self, frame: PhysFrame<Size2MiB>) {
		let (word, bit) = Self::index(frame);
		self.0[word] &= !bit;
	}

	/// Makes all `frames` private and validates them.
	fn validate(&mut self, frames: PhysFrameRangeInclusive<Size2MiB>) {
		let mut entries = [const { PageStateChangeEntry::new() }; Self::MAX_PSC_ENTRIES];
		let mut len = 0;

		for frame in frames {
			if self.contains(frame) {
				continue;
			}

			entries[len] = PageStateChangeEntry::new_for_frame(
				frame,
				PageStateChangeOperation::PageAssignPrivate,
			);
			len += 1;

			if len == Self::MAX_PSC_ENTRIES {
				self.validate_batch(&entries);
				len = 0;
			}
		}

		self.validate_batch(&entries[..len]);
	}

	fn validate_batch(&mut self, entries: &[PageStateChangeEntry]) {
		change_page_state(entries);

		for entry in entries {
			let addr = entry.physical_address();
			pvalidate(
				PageStateChangePageSize::PageSize2MB,
				true,
				VirtAddr::new(addr.as_u64()),
			);
			self.insert(PhysFrame::from_start_address(addr).unwrap());
		}
	}

	/// Shares `frame` with the hypervisor.
	///
	/// # Safety
	///
	/// The frame must be identity mapped, and the caller must own all of its memory.
	unsafe fn make_shared(&mut self, frame: PhysFrame<Size2MiB>) {
		if self.contains(frame) {
			unsafe {
				mapping_utils::make_shared_large::<StaticGhcbManager>(
					frame,
					VirtAddr::new(frame.start_address().as_u64()),
				);
			}
			self.remove(frame);
		} else {
			change_page_state(&[PageStateChangeEntry::new_for_frame(
				frame,
				PageStateChangeOperation::PageAssignShared,
			)]);
		}
	}
}

fn change_page_state(entries: &[PageStateChangeEntry]) {
	if GhcbMsr::get_current_ghcb_address().is_none() {
		change_page_state_via_msr(entries);
		return;
	}

	loop {
		match ChangePageStateRequest::new(entries).execute::<StaticGhcbManager>() {
			Ok(()) => return,
			// The ghcb crate only retries interruptions after progress was made.
			Err(ChangePageStateError::Interrupted(0)) => {}
			Err(err) => panic!("page state change failed: {err:?}"),
		}
	}
}

/// Changes the page state without a GHCB.
///
/// Usefull, if no GHCB is registered yet.
fn change_page_state_via_msr(entries: &[PageStateChangeEntry]) {
	for entry in entries {
		let operation = || match entry.page_operation() {
			PageStateChangeOperation::PageAssignPrivate => PageStateOperation::AssignPrivate,
			PageStateChangeOperation::PageAssignShared => PageStateOperation::AssignShared,
			operation => panic!("{operation:?} is not supported without a GHCB"),
		};
		let pages = match entry.page_size() {
			PageStateChangePageSize::PageSize4KB => 1,
			PageStateChangePageSize::PageSize2MB => Size2MiB::SIZE / Size4KiB::SIZE,
		};

		let start = PhysFrame::<Size4KiB>::from_start_address(entry.physical_address()).unwrap();
		for frame in PhysFrame::range(start, start + pages) {
			// SAFETY: the MSR protocol needs no GHCB, and we only ever run it on our own frames
			let response =
				unsafe { GhcbMsr::execute(PageStateChangeRequest::create(frame, operation())) };
			assert!(
				response.is_successful(),
				"page state change failed: {:?}",
				response.0
			);
		}
	}
}

/// Makes sure that all memory in `range` is private and validated.
///
/// The range is rounded out to 2 MiB frames. Frames that were not validated before must be
/// identity mapped.
pub fn validate_memory(range: Range<PhysAddr>) {
	if range.is_empty() {
		return;
	}

	let frames = PhysFrame::range_inclusive(
		PhysFrame::containing_address(range.start),
		PhysFrame::containing_address(range.end - 1u64),
	);
	VALIDATED_FRAMES.lock().validate(frames);
}

/// Shares a 2 MiB frame with the hypervisor.
///
/// # Safety
///
/// The frame must be identity mapped, and the caller must own all of its memory.
pub unsafe fn make_shared_large(frame: PhysFrame<Size2MiB>) {
	unsafe { VALIDATED_FRAMES.lock().make_shared(frame) }
}
