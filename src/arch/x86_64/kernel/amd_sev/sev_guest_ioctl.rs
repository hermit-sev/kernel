use core::fmt::Debug;
use core::ffi::c_void;
use hermit_sync::Lazy;
use memory_addresses::VirtAddr;
use ghcb::protocols::GhcbProtocolRequest;
use ghcb::protocols::snp_guest_request::SnpGuestRequest;
use ghcb::structures::snp_guest_request::attest::{AttestationRequest, AttestationResponse};
use crate::arch::kernel::amd_sev::StaticGhcbManager;
use crate::fd::ObjectInterface;
use crate::arch::x86_64::kernel::amd_sev::allocations::{cc_blob, shared_pages};

#[derive(Debug)]
pub struct SevGuestIoCtl;

#[repr(C)]
struct IoCtlRequest {
    msg_version: u32,
    req_data: VirtAddr,
    resp_data: VirtAddr,
    fw_error: u32,
    vmm_error: u32
}

impl ObjectInterface for SevGuestIoCtl {
    async fn handle_ioctl(&mut self, cmd: crate::fd::IoCtlCall, argp: &mut [u8]) -> crate::io::Result<()> {
        assert_eq!(cmd.call_type(), 0x53, "this call is not handled by this IOCTL");

        info!("sev-ioctl: command called: {cmd:x?}");

        let argp = argp.as_mut_ptr();
        let arg = argp.cast::<IoCtlRequest>();

        match cmd.call_nr() {
            0x00 /* GetReport */ => {
                info!("sev-ioctl: req_data: {:p}, resp_data: {:p}",
                    unsafe { (*arg).req_data.as_ptr::<u8>() },
                    unsafe { (*arg).resp_data.as_ptr::<u8>() },
                );

                // TODO: ensure addresses are valid
                // TODO: reduce data copies
                let attestation_request = unsafe {
                    (*arg).req_data.as_ptr::<AttestationRequest>()
                };

                let attestation_request = unsafe {
                    (*attestation_request).clone()
                };

                info!("!! will print attestation request");
                info!("sev-ioctl: attestation_request: {:x?}", &attestation_request);

                let request = SnpGuestRequest::new(
                    attestation_request,
                    Lazy::force(&cc_blob::CC_BLOB),
                    Lazy::force(&shared_pages::REQUEST_PAGE),
                    Lazy::force(&shared_pages::RESPONSE_PAGE),
                );
                let attestation_response = request
                    .execute::<StaticGhcbManager>()
                    .expect("failed to obtain attestation");

                info!("!! will print attestation response");
                info!("sev-ioctl: attestation_response: {:x?}", &attestation_response);

                unsafe {
                    let target_ptr = (*arg).resp_data.as_mut_ptr::<AttestationResponse>();
                    target_ptr.write(attestation_response);

                    // TODO
                    (*arg).fw_error = 0;
                    (*arg).vmm_error = 0;
                }
            }
            0x01 /* GetDerivedKey */ => {}
            0x02 /* GetExtReport */ => {}
            _ => panic!("this call is not handled by this IOCTL"),
        }

        Ok(())
    }
}