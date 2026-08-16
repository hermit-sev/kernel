use core::{fmt, iter, ptr};
use core::ffi::CStr;
use core::num::{NonZero, NonZeroUsize};
use align_address::Align;
use hermit_sync::OnceCell;
use linux_boot_params::{BootE820Entry, BootParams, E820Type};
use x86_64::structures::paging::{PageSize, Size4KiB};
use crate::env::MemmapType;
use super::{MemmapEntry, StartInfo};

static START_INFO: OnceCell<BootParams> = OnceCell::new();

#[cfg(feature = "amd-sev")]
const CCBLOB_GUEST_ADDR: NonZeroUsize = {
	use crate::arch::kernel::amd_sev::allocations::cc_blob::SNPCCBlob;

	const ZERO_PAGE_START: usize = 0x7000;
	const CPUID_PAGE_LEN: usize = 0x1000;
	const CCBLOB_LEN: usize = size_of::<SNPCCBlob>();

	// https://github.com/bencw12/rust-hypervisor-firmware/blob/sev-snp-direct-boot/src/fw_cfg.rs#L315
	NonZeroUsize::new((ZERO_PAGE_START + CPUID_PAGE_LEN) - CCBLOB_LEN).unwrap()
};

pub fn start_info() -> &'static impl StartInfo {
	START_INFO.get().unwrap()
}

pub unsafe fn set_boot_params(addr: *const BootParams) {
	let data = unsafe { *addr };
	START_INFO.set(data).unwrap();
}


trait BootParamsExt {
	fn e820_entries(&self) -> &[BootE820Entry];
}

impl BootParamsExt for BootParams {
	fn e820_entries(&self) -> &[BootE820Entry] {
		let e820_entries = self.e820_entries as usize;
		&self.e820_table[..e820_entries]
	}
}


unsafe impl StartInfo for BootParams {
	fn display(&self) -> impl fmt::Display {
		fmt::from_fn(move |f| write!(f, "{self:#x?}"))
	}

	fn bootargs(&self) -> Option<&str> {
		let cmd_line_ptr = self.hdr.cmd_line_ptr as usize;
		let cmdline_size = self.hdr.cmdline_size as usize;

		assert_ne!(cmd_line_ptr, 0, "boot protocol is older than 2.02");
		assert!(cmd_line_ptr.is_aligned_to(Size4KiB::SIZE as usize));

		let ptr = ptr::with_exposed_provenance(cmd_line_ptr);
		let bytes = unsafe { core::slice::from_raw_parts(ptr, cmdline_size) };
		let c_str = CStr::from_bytes_until_nul(bytes).unwrap();

		c_str.to_str().ok()
	}

	#[cfg(feature = "amd-sev")]
	fn cc_blob_addr(&self) -> Option<NonZero<usize>> {
		let addr = self.cc_blob_address as usize;

		if addr > 0 {
			Some(NonZero::new(addr).unwrap())
		} else {
			Some(CCBLOB_GUEST_ADDR.into())
		}
	}

	fn memmap(&self) -> impl Iterator<Item = MemmapEntry> {
		self.e820_entries()
			.iter()
			.filter_map(|entry| {
				let BootE820Entry { addr, size, typ } = *entry;

				Some(MemmapEntry {
					phys_addr: addr as usize,
					len: size as usize,
					ty: typ.try_into().ok()?,
				})
			})
	}

	fn rsdp_addr(&self) -> Option<NonZero<usize>> {
		NonZero::new(self.acpi_rsdp_addr as usize)
	}
}

impl TryFrom<E820Type> for MemmapType {
	type Error = ();

	fn try_from(value: E820Type) -> Result<Self, Self::Error> {
		match value {
			E820Type::Ram => Ok(MemmapType::Ram),
			E820Type::Reserved => Ok(MemmapType::Reserved),
			E820Type::Acpi => Ok(MemmapType::Acpi),
			E820Type::Nvs => Ok(MemmapType::Nvs),
			E820Type::Unusable => Ok(MemmapType::Unusable),
			E820Type::Pmem => Ok(MemmapType::Pmem),
			_ => Err(())
		}
	}
}

