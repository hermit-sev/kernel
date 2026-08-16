use core::arch::{asm, naked_asm};
use core::sync::atomic::Ordering;
use linux_boot_params::BootParams;
use crate::arch::kernel::{CPU_ONLINE, CURRENT_STACK, CURRENT_STACK_ADDRESS};
use crate::config::KERNEL_STACK_SIZE;
use crate::env;
use crate::mm::stack_alloc;
use crate::mm::stack_alloc::MARKER_SIZE;

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start(_params: *mut BootParams) -> ! {
	naked_asm!(
    	// Move the base address of the struct boot_params into `RDI` as first argument to `rust_start`.
    	"mov rdi, rsi",

		// Store current stack pointer to second func. arg. to allow storing in CURRENT_STACK_ADDRESS
		"mov rsi, rsp",

		// Add top stack offset
		"add rsp, {stack_top_offset}",

		// Jump into Rust code
		"jmp short {rust_start}",

		stack_top_offset = const KERNEL_STACK_SIZE - stack_alloc::MARKER_SIZE,
		rust_start = sym rust_start,
	)
}

/// The Rust entry point.
unsafe extern "C" fn rust_start(boot_params: *mut BootParams, stack_addr: *mut u8) -> ! {
	CURRENT_STACK_ADDRESS.store(stack_addr, Ordering::Relaxed);

	unsafe {
        env::set_boot_params(boot_params as *const BootParams);
    }

    crate::rt::boot_processor_main()
}
