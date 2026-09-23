use core::ops::Deref;

use ghcb::msr::GhcbMsr;
use ghcb::structures::ChannelManager;
use ghcb::structures::channel::GhcbChannel;
use hermit_sync::Lazy;

use crate::arch::kernel::amd_sev::mmap::SevAllocator;
use crate::arch::kernel::amd_sev::sev_request_exit;
use crate::arch::kernel::core_local::CoreLocal;

static EFI_GHCB: Lazy<GhcbChannel> =
	Lazy::new(|| unsafe { GhcbChannel::identity_mapped().unwrap() });

/// GHCB setup for uhyve
#[cfg(feature = "uhyve")]
pub mod uhyve {
	use core::ptr;

	use ghcb::msr::GhcbMsr;
	use ghcb::structures::channel::GhcbChannel;
	use ghcb::structures::ghcb_page::GhcbPage;
	use hermit_sync::OnceCell;
	use x86_64::PhysAddr;
	use x86_64::structures::paging::{PhysFrame, Size4KiB};
	use crate::env::StartInfo;
	use super::GHCB_PROTOCOL_VERSION;

	/// The boot processor's GHCB. Brought up before core-local storage exists, and kept as core 0's
	/// GHCB from then on.
	pub(super) static EARLY_GHCB: OnceCell<GhcbChannel> = OnceCell::new();

	/// Brings up the boot processor's GHCB from its FDT-provided page, if present. Must run before
	/// the first GHCB use (i.e. before any output) and after the early #VC handler and
	/// memory-encryption configuration are set up.
	///
	/// Returns `true` if a GHCB was registered.
	pub fn init_early_ghcb() -> bool {
		// SAFETY: nothing has registered a GHCB yet.
		let Some(channel) = (unsafe { register_fdt_ghcb() }) else {
			return false;
		};

		EARLY_GHCB
			.set(channel)
			.unwrap_or_else(|_| panic!("early GHCB already initialized"));

		true
	}

	/// Registers the 4 KiB GHCB page uhyve reserved for the boot processor.
	unsafe fn register_fdt_ghcb() -> Option<GhcbChannel> {
		let gpa = crate::env::start_info().ghcb_addr()?.get() as u64;
		let phys = PhysAddr::new(gpa);
		let frame =
			PhysFrame::<Size4KiB>::from_start_address(phys).expect("GHCB GPA must be page-aligned");
		let page = ptr::with_exposed_provenance_mut::<GhcbPage>(gpa as usize);

		unsafe {
			(*page).set_protocol_version(GHCB_PROTOCOL_VERSION);
			GhcbMsr::register_and_set_ghcb(frame).expect("failed to register GHCB");
			Some(GhcbChannel::new_registered(frame, page))
		}
	}
}

/// The boot processor's GHCB, for as long as its core-local slot is unset.
#[cfg(feature = "uhyve")]
fn boot_processor_ghcb() -> &'static GhcbChannel {
	uhyve::EARLY_GHCB.get().unwrap_or_else(|| EFI_GHCB.deref())
}

#[cfg(not(feature = "uhyve"))]
fn boot_processor_ghcb() -> &'static GhcbChannel {
	EFI_GHCB.deref()
}

#[derive(Debug)]
pub struct StaticGhcbManager;

impl ChannelManager for StaticGhcbManager {
	fn get_channel() -> &'static GhcbChannel {
		let core = CoreLocal::get();
		match core.ghcb.get() {
			Some(ghcb) => ghcb,
			None if core.core_id == 0 => boot_processor_ghcb(),
			None => {
				// We cannot panic because we don't have a GHCB
				sev_request_exit(0x13)
			}
		}
	}
}

/// A channel manager that will always return the boot processor's GHCB, no matter the core used.
/// This should only be used in a panicking context
pub struct EmergencyChannelManager;

impl ChannelManager for EmergencyChannelManager {
	fn get_channel() -> &'static GhcbChannel {
		let ghcb = boot_processor_ghcb();
		unsafe {
			let _ = GhcbMsr::register_and_set_ghcb(ghcb.phys_frame());
		}
		ghcb
	}
}

const GHCB_PROTOCOL_VERSION: u16 = 2;

/// Allocates and registers the GHCB of the current core.
#[cfg(any(feature = "smp", feature = "uhyve"))]
pub fn init_ghcb_for_core() {
	// SAFETY: the GHCB used while booting this core is not used anymore once the core-local one is set
	let channel = unsafe { GhcbChannel::allocate_register::<SevAllocator>(GHCB_PROTOCOL_VERSION) };

	let core = CoreLocal::get();
	if core.ghcb.set(channel).is_err() {
		panic!("GHCB is already initialized!");
	}

	info!("GHCB for core: {:?}", core.ghcb.get().expect("no GHCB set"));
}
