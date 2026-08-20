use core::ptr;

use memory_addresses::VirtAddr;
use uhyve_interface::GuestPhysAddr;
use uhyve_interface::v2::Hypercall;
#[cfg(not(feature = "amd-sev"))]
use uhyve_interface::v2::HypercallAddress;
use uhyve_interface::v2::parameters::*;

use crate::arch::kernel::processor;
use crate::arch::mm::paging::virtual_to_physical;

#[cfg(target_os = "none")]
hermit_entry::define_uhyve_interface_version!(uhyve_interface::UHYVE_INTERFACE_VERSION);

/// perform a SerialWriteBuffer hypercall with `buf` as payload
#[inline]
pub(crate) fn serial_buf_hypercall(buf: &[u8]) {
	let p = SerialWriteBufferParams {
		buf: GuestPhysAddr::new(
			virtual_to_physical(VirtAddr::from_ptr(ptr::from_ref::<[u8]>(buf)))
				.unwrap()
				.as_u64(),
		),
		len: buf.len() as u64,
	};
	uhyve_hypercall(Hypercall::SerialWriteBuffer(&p));
}

/// Calculates the physical address of the struct passed as reference.
#[inline]
#[cfg(not(feature = "amd-sev"))]
fn data_addr<T>(data: &T) -> u64 {
	virtual_to_physical(VirtAddr::from_ptr(ptr::from_ref(data)))
		.unwrap()
		.as_u64()
}

/// Calculates the hypercall data argument
#[inline]
#[cfg(not(feature = "amd-sev"))]
fn hypercall_data(hypercall: &Hypercall<'_>) -> u64 {
	match hypercall {
		// As we are encoding an exit code (max 32 bits) into "an
		// address", and memory_addresses complains if an address
		// has any bits above the 48th one set to 1, we encode
		// potential negative numbers into a u32, then a u64.
		Hypercall::Exit(exit_code) => u64::from((*exit_code) as u32),
		Hypercall::FileClose(data) => data_addr(*data),
		Hypercall::FileLseek(data) => data_addr(*data),
		Hypercall::FileOpen(data) => data_addr(*data),
		Hypercall::FileRead(data) => data_addr(*data),
		Hypercall::FileUnlink(data) => data_addr(*data),
		Hypercall::FileWrite(data) => data_addr(*data),
		Hypercall::Getdents(data) => data_addr(*data),
		Hypercall::FileStat(data) => data_addr(*data),
		Hypercall::FileFstat(data) => data_addr(*data),
		Hypercall::SerialWriteBuffer(data) => data_addr(*data),
		Hypercall::SerialWriteByte(byte) => u64::from(*byte),
		Hypercall::Snapshot(data) => data_addr(*data),
		h => todo!("unimplemented hypercall {h:?}"),
	}
}

#[cfg(all(target_arch = "x86_64", feature = "amd-sev"))]
pub(crate) use sev::uhyve_hypercall;

/// Perform a hypercall to the uhyve hypervisor
#[inline]
#[allow(unused_variables)] // until riscv64 is implemented
#[cfg(not(feature = "amd-sev"))]
pub(crate) fn uhyve_hypercall(hypercall: Hypercall<'_>) {
	let ptr = HypercallAddress::from(&hypercall) as u16;
	let data = hypercall_data(&hypercall);

	#[cfg(target_arch = "x86_64")]
	{
		unsafe {
			use core::arch::asm;
			asm!(
				"out dx, eax",
				in("dx") ptr,
				in("eax") 0x1234u32,
				in("rdi") data,
				options(nostack, preserves_flags)
			);
		}
	}

	#[cfg(target_arch = "aarch64")]
	unsafe {
		use core::arch::asm;
		asm!(
			"str x8, [{ptr:x}]",
			ptr = in(reg) ptr,
			in("x8") data,
			options(nostack),
		);
	}

	#[cfg(target_arch = "riscv64")]
	todo!()
}

pub fn shutdown(error_code: i32) -> ! {
	uhyve_hypercall(Hypercall::Exit(error_code));
	loop {
		processor::halt();
	}
}

#[cfg(all(target_arch = "x86_64", feature = "amd-sev"))]
mod sev {
	use core::ptr;
	use core::sync::atomic::{Ordering, compiler_fence};

	use hermit_sync::Lazy;
	use uhyve_interface::GuestPhysAddr;
	use uhyve_interface::v2::{Hypercall, HypercallAddress};
	use crate::arch::kernel::core_local::core_id;
	use crate::env::FdtStartInfo;

	/// uhyve gives every core one page of hypercall memory, shared with the host at launch.
	const PAGE_SIZE: usize = 0x1000;

	/// The parameter struct occupies the start of the core's page, ...
	const PARAMS_SIZE: usize = 128;

	/// ... the remainder bounces whatever the parameters point to.
	const PAYLOAD_SIZE: usize = PAGE_SIZE - PARAMS_SIZE;

	static HYPERCALL_AREA: Lazy<usize> = Lazy::new(|| {
		crate::env::start_info().fdt()
			.unwrap()
			.find_node("/uhyve,sev")
			.unwrap()
			.property("hypercall_data")
			.unwrap()
			.as_usize()
			.unwrap()
	});

	/// Triggers the hypercall by writing `data` to its MMIO address.
	unsafe fn mmio_write(addr: u16, data: u64) {
		use ghcb::protocols::mmio::MmioPtr;
		use x86_64::PhysAddr;

		use crate::arch::kernel::amd_sev::StaticGhcbManager;

		unsafe {
			MmioPtr::<u64, StaticGhcbManager>::new(PhysAddr::new(addr as u64)).write_volatile(data);
		}
	}

	/// Length of the NUL-terminated path at `addr`, including the terminator.
	fn path_len(addr: GuestPhysAddr) -> usize {
		let path: *mut u8 = ptr::with_exposed_provenance_mut(addr.as_u64() as usize);
		let mut len = 0;
		// SAFETY: as long as addr is core-local, we don't have UB.
		while unsafe { path.add(len).read_volatile() } != 0 {
			len += 1;
			assert!(
				len < PAYLOAD_SIZE,
				"hypercall path does not fit the shared page"
			);
		}
		len + 1
	}

	/// The calling core's hypercall page.
	struct HypercallPage(usize);
	impl HypercallPage {
		fn get() -> Self {
			Self(*HYPERCALL_AREA + core_id() as usize * PAGE_SIZE)
		}

		/// The payload area past the parameter slot.
		fn payload_ptr(&self) -> *mut u8 {
			ptr::with_exposed_provenance_mut(self.0 + PARAMS_SIZE)
		}

		/// The parameter slot at the head of the page.
		fn params_ptr<T>(&self) -> *mut T {
			const {
				assert!(
					size_of::<T>() <= PARAMS_SIZE,
					"hypercall parameters do not fit the shared page"
				);
			}
			ptr::with_exposed_provenance_mut(self.0)
		}

		/// Copies `len` bytes of guest memory at `addr` into the payload area and returns the
		/// address of the copy.
		fn stage_payload(&self, addr: GuestPhysAddr, len: usize) -> GuestPhysAddr {
			let staged = self.reserve_payload(len);
			// SAFETY: `addr` is identity-mapped guest RAM, the destination is the core's own
			// payload area and `reserve_payload` checked that `len` fits it.
			unsafe {
				ptr::copy_nonoverlapping(
					ptr::with_exposed_provenance_mut(addr.as_u64() as usize),
					self.payload_ptr(),
					len,
				);
			}
			staged
		}

		/// Hands out the payload area for the hypervisor to fill in with up to `len` bytes.
		fn reserve_payload(&self, len: usize) -> GuestPhysAddr {
			// TODO: fall back to a larger shared buffer instead of panicing.
			assert!(
				len <= PAYLOAD_SIZE,
				"hypercall payload of {len} bytes does not fit the shared page"
			);
			GuestPhysAddr::new((self.0 + PARAMS_SIZE) as u64)
		}
	}

	/// Copies `params` into the core's shared parameter slot, triggers the hypercall and reads the slot back.
	fn perform_hypercall<T: Copy>(page: &HypercallPage, addr: u16, params: &mut T) {
		let slot = page.params_ptr::<T>();
		// SAFETY: the slot is the head of the core's own shared page, large enough for `T`.
		unsafe {
			slot.write_volatile(*params);
			compiler_fence(Ordering::Release);
			mmio_write(addr, page.0 as u64);
			compiler_fence(Ordering::Acquire);
			*params = slot.read_volatile();
		}
	}

	/// Issues a hypercall whose parameters point at a buffer the hypervisor reads.
	fn hypercall_with_buffer_input<T: Copy>(
		page: &HypercallPage,
		addr: u16,
		params: &mut T,
		buf: GuestPhysAddr,
		len: usize,
		set_buf: impl Fn(&mut T, GuestPhysAddr),
	) {
		let mut staged = *params;
		set_buf(&mut staged, page.stage_payload(buf, len));
		perform_hypercall(page, addr, &mut staged);
		set_buf(&mut staged, buf);
		*params = staged;
	}

	/// Issues a hypercall whose parameters point at a buffer of `len` bytes the hypervisor fills in.
	fn hypercall_with_buffer_output<T: Copy>(
		page: &HypercallPage,
		addr: u16,
		params: &mut T,
		buf: GuestPhysAddr,
		len: usize,
		set_buf: impl Fn(&mut T, GuestPhysAddr),
		out_len: impl Fn(&T) -> usize,
	) {
		let mut staged = *params;
		set_buf(&mut staged, page.reserve_payload(len));
		perform_hypercall(page, addr, &mut staged);
		set_buf(&mut staged, buf);
		*params = staged;

		// Clamped to `len`, so a hypervisor reporting more than it was offered cannot overrun the caller's buffer or read past the payload area.
		let written = out_len(params).min(len);
		// SAFETY: `buf` is identity-mapped guest RAM of at least `len` bytes.
		unsafe {
			ptr::copy_nonoverlapping(
				page.payload_ptr(),
				ptr::with_exposed_provenance_mut(buf.as_u64() as usize),
				written,
			);
		}
	}

	pub(crate) fn uhyve_hypercall(hypercall: Hypercall<'_>) {
		let page = HypercallPage::get();
		let addr = HypercallAddress::from(&hypercall) as u16;

		match hypercall {
			Hypercall::Exit(exit_code) => unsafe {
				mmio_write(addr, u64::from(exit_code as u32));
			},
			Hypercall::SerialWriteByte(byte) => unsafe {
				mmio_write(addr, u64::from(byte));
			},
			Hypercall::FileClose(params) => perform_hypercall(&page, addr, params),
			Hypercall::FileLseek(params) => perform_hypercall(&page, addr, params),
			Hypercall::FileOpen(params) => {
				let name = params.name;
				hypercall_with_buffer_input(
					&page,
					addr,
					params,
					name,
					path_len(name),
					|p, staged| {
						p.name = staged;
					},
				);
			}
			Hypercall::FileUnlink(params) => {
				let name = params.name;
				hypercall_with_buffer_input(
					&page,
					addr,
					params,
					name,
					path_len(name),
					|p, staged| {
						p.name = staged;
					},
				);
			}
			Hypercall::FileWrite(params) => {
				let (buf, len) = (params.buf, params.len as usize);
				hypercall_with_buffer_input(&page, addr, params, buf, len, |p, staged| {
					p.buf = staged
				});
			}
			Hypercall::SerialWriteBuffer(params) => {
				let mut params = *params;
				let (buf, len) = (params.buf, params.len as usize);
				hypercall_with_buffer_input(&page, addr, &mut params, buf, len, |p, staged| {
					p.buf = staged
				});
			}
			Hypercall::FileRead(params) => {
				let (buf, len) = (params.buf, params.len as usize);
				hypercall_with_buffer_output(
					&page,
					addr,
					params,
					buf,
					len,
					|p, staged| p.buf = staged,
					// Negative `ret` is an errno.
					|p| p.ret.max(0) as usize,
				);
			}

			h => todo!("unimplemented hypercall {h:?}"),
		}
	}
}
