use core::alloc::Layout;
use core::ptr::{self, NonNull};

use align_address::Align;
use free_list::PageLayout;
use memory_addresses::{PhysAddr, VirtAddr};
use talc::base::binning::Binning;
use talc::base::Talc;
use talc::source::Source;

use super::ALLOCATOR;
use crate::arch::kernel::amd_sev::validate_memory;
use crate::arch::mm::paging::{
	self, LargePageSize, PageSize, PageTableEntryFlags, PageTableEntryFlagsExt,
};
use crate::mm::FrameAlloc;

const UNIT: usize = LargePageSize::SIZE as usize;
const INITIAL_SIZE: usize = 4 * UNIT;
const MAX_GROW_SIZE: usize = 64 << 20;

/// A heap [`Source`] that maps and validates more memory whenever the heap runs out of memory.
#[derive(Debug)]
pub struct LazyHeapSource {
	start: usize,
	current_end: usize,
	limit: usize,
	phys_start: usize,
}

impl LazyHeapSource {
	pub const fn new() -> Self {
		Self {
			start: 0,
			current_end: 0,
			limit: 0,
			phys_start: 0,
		}
	}
}

/// Initializes the heap in the virtual range `start..start + size` but maps only a small part of it.
pub fn init(start: VirtAddr, size: usize) {
	assert!(start.is_aligned_to(UNIT as u64));
	let size = size.align_down(UNIT);
	let layout = PageLayout::from_size_align(size, UNIT).unwrap();
	let frames = FrameAlloc::allocate_unvalidated(layout).unwrap();
	let phys_start = frames.start();

	let initial_heap_size = INITIAL_SIZE.min(size);
	map_and_validate(start, PhysAddr::from(phys_start), initial_heap_size);

	let mut talc = ALLOCATOR.lock();
	let end = unsafe { talc.claim(start.as_mut_ptr(), initial_heap_size) }.unwrap();
	let end = end.as_ptr().expose_provenance();
	assert!(end.is_multiple_of(UNIT));
	talc.source = LazyHeapSource {
		start: start.as_usize(),
		current_end: end,
		limit: start.as_usize() + size,
		phys_start,
	};
	drop(talc);

	info!(
		"Heap is located at {start:p}..{:p} ({initial_heap_size} Bytes mapped), backed by {phys_start:#x}..{:#x}",
		start + size,
		phys_start + size
	);
}

/// Validates, maps and zeroes the `size` bytes at `phys_addr` to `virt_addr`.
fn map_and_validate(virt_addr: VirtAddr, phys_addr: PhysAddr, size: usize) {
	validate_memory(phys_addr.into()..(phys_addr + size as u64).into());

	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().execute_disable();
	flags.set_encrypted(true);
	paging::map::<LargePageSize>(virt_addr, phys_addr, size / UNIT, flags);

	unsafe {
		virt_addr.as_mut_ptr::<u8>().write_bytes(0, size);
	}
}

// SAFETY: `acquire` neither allocates on the heap nor logs.
unsafe impl Source for LazyHeapSource {
	fn acquire<B: Binning>(talc: &mut Talc<Self, B>, layout: Layout) -> Result<(), ()> {
		let Self {
			start,
			current_end,
			limit,
			phys_start,
		} = talc.source;
		if current_end == 0 {
			return Err(());
		}

		// Leave room for chunk metadata and alignment padding.
		let needed = layout
			.size()
			.checked_add(layout.align() + 4 * size_of::<usize>())
			.and_then(|size| size.checked_next_multiple_of(UNIT))
			.ok_or(())?;
		let size = needed
			.max((current_end - start).min(MAX_GROW_SIZE))
			.min(limit - current_end);
		if size < needed {
			return Err(());
		}

		let phys_end = PhysAddr::new((phys_start + (current_end - start)) as u64);
		map_and_validate(VirtAddr::new(current_end as u64), phys_end, size);

		let heap_end = NonNull::new(ptr::with_exposed_provenance_mut(current_end)).unwrap();
		let new_end = unsafe {
			talc.extend(
				heap_end,
				ptr::with_exposed_provenance_mut(current_end + size),
			)
		};
		talc.source.current_end = new_end.as_ptr().expose_provenance();

		Ok(())
	}
}
