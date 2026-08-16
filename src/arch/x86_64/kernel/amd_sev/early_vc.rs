//! Minimal #VC exception handling for early boot.

use ghcb::make_vc_handler;
use ghcb::msr::GhcbMsr;
use ghcb::msr::cpuid::CpuidRegister;
use ghcb::protocols::GhcbProtocolRequest;
use ghcb::protocols::cpuid::CpuIdRequest;
use ghcb::structures::ChannelManager;
use ghcb::vc_handler::VcHandler;
use ghcb::vc_handler::exits::SvmInterceptCode;
use ghcb::vc_handler::structures::instruction_parser::InstructionData;
use ghcb::vc_handler::structures::opcodes::opcode::KnownOpcode;
use ghcb::vc_handler::structures::stack_frame::VCInterruptStackFrame;
use hermit_sync::InterruptSpinMutex;
use x86_64::registers::control::{Cr4, Cr4Flags};
use x86_64::registers::xcontrol::XCr0;
use x86_64::set_general_handler;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

static EARLY_IDT: InterruptSpinMutex<InterruptDescriptorTable> =
	InterruptSpinMutex::new(InterruptDescriptorTable::new());

/// Reports any non-#VC exception to the hypervisor when nothing can be printed yet.
fn early_abort(_stack_frame: InterruptStackFrame, index: u8, _error_code: Option<u64>) {
	pub const EARLY_TERM_REASON_SET: u8 = 0xe;
	GhcbMsr::terminate(EARLY_TERM_REASON_SET, index)
}

/// Loads a minimal IDT for early boot
pub fn install_early_handler() {
	let mut idt = EARLY_IDT.lock();
	set_general_handler!(&mut *idt, early_abort, 0..32);
	idt.vmm_communication_exception
		.set_handler_fn(early_vc_exception);
	drop(idt);

	unsafe {
		(*EARLY_IDT.data_ptr()).load_unsafe();
	}
}

struct EarlyCpuidHandler;

impl VcHandler for EarlyCpuidHandler {
	fn handle(&self, frame: &mut VCInterruptStackFrame, idata: &mut InstructionData) {
		assert_eq!(idata.operation(), KnownOpcode::CPUID);

		let function = (frame.registers.rax & 0xffff_ffff) as u32;
		let subleaf = (frame.registers.rcx & 0xffff_ffff) as u32;

		let (eax, ebx, ecx, edx) = match function {
			// Leaf 0xD (XSAVE enumeration) depends on the subleaf and on XCR0, so we need a ghcb there.
			0x0000_000d => cpuid_via_ghcb(function, subleaf),
			// Other CPUID functions are called before a ghcb is present.
			_ => cpuid_via_msr(function),
		};

		frame.registers.rax = eax as u64;
		frame.registers.rbx = ebx as u64;
		frame.registers.rcx = ecx as u64;
		frame.registers.rdx = edx as u64;
	}
}

#[cfg(feature = "linux-boot")]
type EarlyGhcbManager = crate::arch::kernel::amd_sev::allocations::ghcb::EmergencyChannelManager;

#[cfg(feature = "uhyve")]
type EarlyGhcbManager = crate::arch::kernel::amd_sev::allocations::ghcb::StaticGhcbManager;

fn cpuid_via_ghcb(function: u32, subleaf: u32) -> (u32, u32, u32, u32) {
	let xcr0 = if Cr4::read().contains(Cr4Flags::OSXSAVE) {
		XCr0::read_raw()
	} else {
		0
	};

	let result = EarlyGhcbManager::get_channel().with_ghcb(|mut ghcb| {
		CpuIdRequest::for_leaf(function)
			.with_subleaf(subleaf)
			.with_xcr0(xcr0)
			.execute_request(&mut ghcb)
	});
	(result.eax, result.ebx, result.ecx, result.edx)
}

fn cpuid_via_msr(function: u32) -> (u32, u32, u32, u32) {
	let query = |register| (GhcbMsr::cpuid(function, register).into_bits() >> 32) as u32;
	(
		query(CpuidRegister::Eax),
		query(CpuidRegister::Ebx),
		query(CpuidRegister::Ecx),
		query(CpuidRegister::Edx),
	)
}

struct EarlyTerminateHandler;

impl VcHandler for EarlyTerminateHandler {
	fn handle(&self, frame: &mut VCInterruptStackFrame, _idata: &mut InstructionData) {
		const EARLY_VC_REASON_SET: u8 = 0xd;
		GhcbMsr::terminate(EARLY_VC_REASON_SET, frame.error_code as u8)
	}
}

make_vc_handler!(early_vc_exception;
	SvmInterceptCode::CPUID => EarlyCpuidHandler,
	_ => EarlyTerminateHandler
);
