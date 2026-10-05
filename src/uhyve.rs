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
	use alloc::boxed::Box;
	use core::mem::MaybeUninit;
	use core::ptr;
	use core::sync::atomic::{Ordering, compiler_fence};

	use hermit_sync::Lazy;
	use uhyve_interface::GuestPhysAddr;
	use uhyve_interface::v2::parameters::{
		FileAttr, GetdentResult, ReadParams, SerialWriteBufferParams, WriteParams,
	};
	use uhyve_interface::v2::{Hypercall, HypercallAddress};
	use crate::arch::kernel::core_local::core_id;
	use crate::env::FdtStartInfo;
	use crate::mm::device_alloc::DeviceAlloc;

	/// uhyve gives every core one page of hypercall memory, shared with the host at launch.
	const PAGE_SIZE: usize = 0x1000;

	/// The parameter struct occupies the start of the core's page, ...
	const PARAMS_SIZE: usize = 128;

	/// ... the remainder bounces whatever the parameters point to.
	const PAYLOAD_SIZE: usize = PAGE_SIZE - PARAMS_SIZE;

	/// Larger payloads are bounced through a temporary shared buffer of at most this size and
	/// split into several hypercalls if they exceed it.
	const MAX_BOUNCE: usize = 256 * 1024;

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
				len < MAX_BOUNCE,
				"hypercall path does not fit the bounce buffer"
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
	}

	/// Memory shared with the hypervisor that hypercall payloads are bounced through.
	enum BounceMemory {
		/// The payload area of the core's hypercall page, at this address.
		Page(usize),
		Shared(Box<[MaybeUninit<u8>], DeviceAlloc>),
	}

	impl BounceMemory {
		/// Returns a buffer for up to `len` bytes, capped at [`MAX_BOUNCE`].
		fn new(page: &HypercallPage, len: usize) -> Self {
			if len <= PAYLOAD_SIZE {
				Self::Page(page.0 + PARAMS_SIZE)
			} else {
				Self::Shared(Box::new_uninit_slice_in(len.min(MAX_BOUNCE), DeviceAlloc))
			}
		}

		fn capacity(&self) -> usize {
			match self {
				Self::Page(_) => PAYLOAD_SIZE,
				Self::Shared(buf) => buf.len(),
			}
		}

		fn as_mut_ptr(&mut self) -> *mut u8 {
			match self {
				Self::Page(addr) => ptr::with_exposed_provenance_mut(*addr),
				Self::Shared(buf) => buf.as_mut_ptr().cast(),
			}
		}

		/// The address the hypervisor accesses the buffer at.
		fn guest_addr(&mut self) -> GuestPhysAddr {
			match self {
				Self::Page(addr) => GuestPhysAddr::new(*addr as u64),
				Self::Shared(buf) => {
					GuestPhysAddr::new(DeviceAlloc.phys_addr_from(buf.as_mut_ptr()).as_u64())
				}
			}
		}

		/// Copies `len` bytes of guest memory at `src` into the buffer, starting at `offset`.
		fn copy_from(&mut self, offset: usize, src: GuestPhysAddr, len: usize) {
			assert!(offset + len <= self.capacity());
			// SAFETY: `src` is identity-mapped guest RAM and the buffer holds `offset + len` bytes.
			unsafe {
				ptr::copy_nonoverlapping(
					ptr::with_exposed_provenance(src.as_u64() as usize),
					self.as_mut_ptr().add(offset),
					len,
				);
			}
		}

		/// Copies the first `len` bytes of the buffer to guest memory at `dst`.
		fn drain(&mut self, dst: GuestPhysAddr, len: usize) {
			assert!(len <= self.capacity());
			// SAFETY: `dst` is identity-mapped guest RAM of at least `len` bytes and the buffer
			// holds `len` bytes.
			unsafe {
				ptr::copy_nonoverlapping(
					self.as_mut_ptr(),
					ptr::with_exposed_provenance_mut(dst.as_u64() as usize),
					len,
				);
			}
		}
	}

	#[derive(PartialEq, Eq)]
	enum Direction {
		/// The hypervisor reads the buffer.
		ToHost,
		/// The hypervisor fills in the buffer.
		FromHost,
	}

	/// Bounces the `len` bytes at `buf` through [`BounceMemory`] until all bytes are 
	/// transferred or a chunk comes back short or fails.
	///
	/// Returns the total bytes transferred, or the errno if the first chunk fails.
	fn bounce_chunked(
		page: &HypercallPage,
		buf: GuestPhysAddr,
		len: usize,
		direction: Direction,
		mut hypercall: impl FnMut(GuestPhysAddr, usize) -> i64,
	) -> i64 {
		let mut memory = BounceMemory::new(page, len);
		let mut done = 0;
		loop {
			let chunk_buf = GuestPhysAddr::new(buf.as_u64() + done as u64);
			let chunk_len = (len - done).min(memory.capacity());
			if direction == Direction::ToHost {
				memory.copy_from(0, chunk_buf, chunk_len);
			}
			let ret = hypercall(memory.guest_addr(), chunk_len);
			if ret < 0 {
				return if done == 0 { ret } else { done as i64 };
			}
			// Clamped, so a hypervisor reporting more than it was offered cannot overrun the
			// caller's buffer.
			let ret = (ret as usize).min(chunk_len);
			if direction == Direction::FromHost {
				memory.drain(chunk_buf, ret);
			}
			done += ret;
			if ret < chunk_len || done == len {
				return done as i64;
			}
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

	/// Issues a hypercall whose parameters point at the NUL-terminated path `name`.
	fn hypercall_with_path<T: Copy>(
		page: &HypercallPage,
		addr: u16,
		params: &mut T,
		name: GuestPhysAddr,
		set_name: impl Fn(&mut T, GuestPhysAddr),
	) {
		let len = path_len(name);
		let mut memory = BounceMemory::new(page, len);
		memory.copy_from(0, name, len);
		set_name(params, memory.guest_addr());
		perform_hypercall(page, addr, params);
		set_name(params, name);
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
				hypercall_with_path(&page, addr, params, params.name, |p, name| p.name = name);
			}
			Hypercall::FileUnlink(params) => {
				hypercall_with_path(&page, addr, params, params.name, |p, name| p.name = name);
			}
			Hypercall::FileWrite(params) => {
				let orig = *params;
				params.ret = bounce_chunked(
					&page,
					orig.buf,
					orig.len as usize,
					Direction::ToHost,
					|buf, len| {
						let mut chunk = WriteParams { buf, len: len as u64, ..orig };
						perform_hypercall(&page, addr, &mut chunk);
						chunk.ret
					},
				);
			}
			Hypercall::SerialWriteBuffer(params) => {
				bounce_chunked(
					&page,
					params.buf,
					params.len as usize,
					Direction::ToHost,
					|buf, len| {
						let mut chunk = SerialWriteBufferParams { buf, len: len as u64 };
						perform_hypercall(&page, addr, &mut chunk);
						len as i64
					},
				);
			}
			Hypercall::FileRead(params) => {
				let orig = *params;
				params.ret = bounce_chunked(
					&page,
					orig.buf,
					orig.len as usize,
					Direction::FromHost,
					|buf, len| {
						let mut chunk = ReadParams { buf, len: len as u64, ..orig };
						perform_hypercall(&page, addr, &mut chunk);
						chunk.ret
					},
				);
			}
			Hypercall::FileFstat(params) => {
				let attr = params.attr;
				let len = size_of::<FileAttr>();
				let mut memory = BounceMemory::new(&page, len);
				params.attr = memory.guest_addr();
				perform_hypercall(&page, addr, params);
				params.attr = attr;
				memory.drain(attr, len);
			}
			Hypercall::FileStat(params) => {
				let orig = *params;
				let attr_len = size_of::<FileAttr>();
				let name_len = path_len(orig.name);
				let mut memory = BounceMemory::new(&page, attr_len + name_len);
				memory.copy_from(attr_len, orig.name, name_len);
				let bounce = memory.guest_addr();
				params.attr = bounce;
				params.name = GuestPhysAddr::new(bounce.as_u64() + attr_len as u64);
				perform_hypercall(&page, addr, params);
				params.attr = orig.attr;
				params.name = orig.name;
				memory.drain(orig.attr, attr_len);
			}
			Hypercall::Getdents(params) => {
				let orig = *params;
				let mut memory = BounceMemory::new(&page, orig.len as usize);
				let len = (orig.len as usize).min(memory.capacity());
				params.buf = memory.guest_addr();
				params.len = len as u64;
				perform_hypercall(&page, addr, params);
				params.buf = orig.buf;
				params.len = orig.len;
				if let GetdentResult::Success(written) = params.ret {
					memory.drain(orig.buf, (written as usize).min(len));
				}
			}

			h => todo!("unimplemented hypercall {h:?}"),
		}
	}
}
