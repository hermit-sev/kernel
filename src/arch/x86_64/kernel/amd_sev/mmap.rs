use crate::mm::{virtual_to_physical, FrameAlloc, PageAlloc, PageRangeAllocator};
use free_list::PageLayout;
use ghcb::mapping::PhysicalAllocator;
use x86_64::structures::paging::{FrameAllocator, PageSize, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

pub struct SevAllocator;

const SIZE_4KIB: usize = 0x1000;

const_assert_eq!(SIZE_4KIB, Size4KiB::SIZE as usize);

unsafe impl PhysicalAllocator for SevAllocator {
    fn allocate_zeroed(flags: PageTableFlags) -> Option<(PhysFrame<Size4KiB>, VirtAddr)> {
        // Allocate a frame
        let frame: PhysFrame<Size4KiB> = FrameAlloc.allocate_frame()?;
        let identity_mapping = memory_addresses::VirtAddr::new(frame.start_address().as_u64());

        if !flags.is_encrypted() {
            unsafe {
                ghcb::mapping::mapping_utils::make_shared(frame, identity_mapping.into())
            }
        }

        // Clear any identity mapping
        crate::arch::mm::paging::unmap::<Size4KiB>(identity_mapping, 1);

        // Map to new virtual address
        let layout = PageLayout::from_size(SIZE_4KIB).unwrap();
        let range = PageAlloc::allocate(layout).ok()?;
        let start = u64::try_from(range.start()).unwrap();
        let start = VirtAddr::new(start);

        crate::arch::mm::paging::map::<Size4KiB>(start.into(), frame.start_address().into(), 1, flags);

        unsafe {
            // Zero the memory
            start.as_mut_ptr::<u8>().write_bytes(0, SIZE_4KIB)
        }

        Some((frame, start))
    }

    unsafe fn deallocate(addr: VirtAddr) {
        assert!(addr.is_aligned(Size4KiB::SIZE));

        let physical_addr = virtual_to_physical(addr.into())
            .expect("failed to resolve address");

        // Remove mapping
        crate::arch::mm::paging::unmap::<Size4KiB>(addr.into(), 1);

        // Free physical frame
        let start = usize::try_from(physical_addr.as_u64()).unwrap();
        let range = free_list::PageRange::from_start_len(start, SIZE_4KIB).unwrap();

        // Re-add an identity mapping for the frame
        crate::arch::mm::paging::identity_map::<Size4KiB>(physical_addr.into());

        // Make frame private under identity mapping
        ghcb::mapping::mapping_utils::make_private(
            PhysFrame::from_start_address(physical_addr.into()).unwrap(),
            VirtAddr::new(physical_addr.as_u64()),
        );

        // Give back the frame
        unsafe {
            FrameAlloc::deallocate(range)
        }
    }
}
