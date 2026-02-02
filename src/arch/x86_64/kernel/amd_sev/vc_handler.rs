use ghcb::make_vc_handler;
use ghcb::vc_handler::exits::SvmInterceptCode;
use ghcb::vc_handler::handlers::{handler_cpuid, handler_ioio, handler_mmio, handler_msr, handler_rmp_invalid};
use crate::arch::x86_64::kernel::amd_sev::allocations::cc_blob::cc_blob;
use crate::arch::kernel::amd_sev::StaticGhcbManager;
use crate::arch::kernel::core_local::increment_irq_counter;
use crate::arch::mm::paging::identity_mapped_page_table;

make_vc_handler!(vmm_interrupt_exception;
    SvmInterceptCode::IOIO => handler_ioio::IoIoHandler::<StaticGhcbManager>::new(),
    SvmInterceptCode::CPUID => handler_cpuid::CpuIdHandler::<StaticGhcbManager, _>::new(cc_blob()),
    SvmInterceptCode::MSR => handler_msr::MsrHandler::<StaticGhcbManager>::new(),
    SvmInterceptCode::NPF => handler_mmio::MmioHandler::<StaticGhcbManager, _>::new(
        &(unsafe { identity_mapped_page_table() })
    ),
    SvmInterceptCode::PageNotValidated => handler_rmp_invalid::RmpInvalidHandler::new(
        &(unsafe { identity_mapped_page_table() })
    );
    pre_handling (stack_frame, _exit_code) {
        log::debug!("VC# HANDLE: {stack_frame:#?}");
        increment_irq_counter(29);
    };
    post_handling (stack_frame, _exit_code) {
        log::debug!("VC# handle done - return address: {:#?}", stack_frame.exception.instruction_pointer);
    }
);


