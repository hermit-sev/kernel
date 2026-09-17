use core::alloc::Layout;
use core::ptr::{self, NonNull};

use align_address::Align;
use free_list::PageLayout;
use memory_addresses::{PhysAddr, VirtAddr};
use talc::base::Talc;
use talc::base::binning::Binning;
use talc::source::Source;

use super::ALLOCATOR;
use crate::arch::mm::paging::{self, LargePageSize, PageSize, PageTableEntryFlags, PageTableEntryFlagsExt};
use crate::mm::{FrameAlloc, PageRangeAllocator};

const UNIT: usize = LargePageSize::SIZE as usize;
const INITIAL_SIZE: usize = 4 * UNIT;
const MAX_GROW_SIZE: usize = 64 << 20;

/// A heap [`Source`] that maps and validates more memory whenever the heap runs out of memory.
#[derive(Debug)]
pub struct LazyHeapSource {
	start: usize,
	end: usize,
	limit: usize,
}

impl LazyHeapSource {
	pub const fn new() -> Self {
		Self {
			start: 0,
			end: 0,
			limit: 0,
		}
	}
}

/// Initializes the heap in the virtual range `start..start + size` but maps only a small part of it.
pub fn init(start: VirtAddr, size: usize) {
	assert!(start.is_aligned_to(UNIT as u64));
	let mapped = map(start, INITIAL_SIZE.min(size.align_down(UNIT)));

	let mut talc = ALLOCATOR.lock();
	let end = unsafe { talc.claim(start.as_mut_ptr(), mapped) }.unwrap();
	let end = end.as_ptr().expose_provenance();
	assert!(end.is_multiple_of(UNIT));
	talc.source = LazyHeapSource {
		start: start.as_usize(),
		end,
		limit: start.as_usize() + size,
	};
	drop(talc);

	info!(
		"Heap is located at {start:p}..{:p} ({mapped} Bytes mapped)",
		start + size
	);
}

/// Maps up to `size` bytes at `virt_addr` and returns the number of mapped bytes.
///
/// Neither allocates on the heap nor logs, unless trace logging is enabled.
fn map(virt_addr: VirtAddr, size: usize) -> usize {
	let count = size / UNIT;

	// Contiguous frames are validated with a single page state change request.
	let layout = PageLayout::from_size_align(size, UNIT).unwrap();
	if let Ok(frames) = FrameAlloc::allocate(layout) {
		let mut flags = PageTableEntryFlags::empty();
		flags.normal().writable().execute_disable();
		flags.set_encrypted(true);
		paging::map::<LargePageSize>(virt_addr, PhysAddr::from(frames.start()), count, flags);
		return size;
	}

	match paging::map_heap::<LargePageSize>(virt_addr, count) {
		Ok(()) => size,
		Err(mapped) => mapped * UNIT,
	}
}

// SAFETY: `acquire` neither allocates on the heap nor logs.
unsafe impl Source for LazyHeapSource {
	fn acquire<B: Binning>(talc: &mut Talc<Self, B>, layout: Layout) -> Result<(), ()> {
		let Self { start, end, limit } = talc.source;
		if end == 0 {
			return Err(());
		}

		// Leave room for chunk metadata and alignment padding.
		let needed = layout
			.size()
			.checked_add(layout.align() + 4 * size_of::<usize>())
			.and_then(|size| size.checked_next_multiple_of(UNIT))
			.ok_or(())?;
		let size = needed.max((end - start).min(MAX_GROW_SIZE)).min(limit - end);
		if size < needed {
			return Err(());
		}

		let mapped = map(VirtAddr::new(end as u64), size);
		if mapped == 0 {
			return Err(());
		}

		let heap_end = NonNull::new(ptr::with_exposed_provenance_mut(end)).unwrap();
		let new_end = unsafe { talc.extend(heap_end, ptr::with_exposed_provenance_mut(end + mapped)) };
		talc.source.end = new_end.as_ptr().expose_provenance();

		Ok(())
	}
}
