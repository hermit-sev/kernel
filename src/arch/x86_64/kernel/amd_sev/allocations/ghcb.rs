use core::ops::Deref;
use hermit_sync::Lazy;
use ghcb::msr::GhcbMsr;
use ghcb::structures::channel::GhcbChannel;
use ghcb::structures::ChannelManager;

use crate::arch::kernel::amd_sev::mmap::SevAllocator;
use crate::arch::kernel::amd_sev::sev_request_exit;
use crate::arch::kernel::core_local::CoreLocal;

static EFI_GHCB: Lazy<GhcbChannel> = Lazy::new(|| unsafe {
	GhcbChannel::identity_mapped().unwrap()
});

#[derive(Debug)]
pub struct StaticGhcbManager;

impl ChannelManager for StaticGhcbManager {
	fn get_channel() -> &'static GhcbChannel {
		let core = CoreLocal::get();
		match core.ghcb.get() {
			Some(allocated_ghcb) => allocated_ghcb,
			None if core.core_id == 0 => EFI_GHCB.deref(),
			None => {
				// We cannot panic because we don't have a GHCB
				sev_request_exit(0x13)
			},
		}
	}
}

/// A channel manager that will always return the EFI GHCB, no matter the core used.
/// This should only be used in a panicking context
pub struct EmergencyChannelManager;

impl ChannelManager for EmergencyChannelManager {
	fn get_channel() -> &'static GhcbChannel {
		let ghcb = EFI_GHCB.deref();
		unsafe {
			let _ = GhcbMsr::register_and_set_ghcb(ghcb.phys_frame());
		}
		ghcb
	}
}

const GHCB_PROTOCOL_VERSION: u16 = 2;

pub fn init_ghcb_for_core() {
	let core = CoreLocal::get();
	if core.ghcb.get().is_some() {
		panic!("GHCB is already initialized!");
	}

	let allocated = unsafe {
		// SAFETY: the interrupt handler is not yet set, and we immediately set the core GHCB
		GhcbChannel::allocate_register::<SevAllocator>(GHCB_PROTOCOL_VERSION)
	};
	if let Err(_) = core.ghcb.set(allocated) {
		unreachable!("Race condition: GHCB is already initialized! (was not a few lines ago!)");
	}

	let allocated = core.ghcb.get().expect("no GHCB set");
	info!("Allocated GHCB for core: {:?}", allocated);
}
