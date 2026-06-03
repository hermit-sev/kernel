//! This module loads and exposes the Confidential Computing Blob, as built in memory by firmware.
//!
//! The structure is defined in Edk2 here: https://github.com/tianocore/edk2/blob/master/OvmfPkg/Include/Guid/ConfidentialComputingSevSnpBlob.h#L21
//! It is exposed in a specific configuration UUID, which is read by the bootloader and exposed in the FDT.
//!
//! The different pages are defined in AMD SEV Secure Nested Paging Firmware ABI Specification (doc ID 56860).
//! https://www.amd.com/content/dam/amd/en/documents/epyc-technical-docs/specifications/56860.pdf

use crate::env;
use core::ptr;
use hermit_sync::{Lazy, RwSpinLock};
use core::ops::DerefMut;
use ghcb::structures::snp_cpuid_page::CPUIDPage;
use ghcb::structures::snp_secrets_page::{SNPSecretsPage, SecretsPageAccessor};
use ghcb::vc_handler::handlers::handler_cpuid::CpuIdPageAccessor;
use crate::env::StartInfo;

pub struct ConfidentialComputingBlob {
    secrets: SNPSecrets,
    cpuid: SNPCpuIdValues
}

impl ConfidentialComputingBlob {
    fn new() -> Option<ConfidentialComputingBlob> {
        let cc_blob: &SNPCCBlob = env::start_info().cc_blob_addr().and_then(|cc_blob| {
            info!("AMD-SEV: EFI CC Blob detected at {cc_blob:#x}");
            let p = ptr::with_exposed_provenance::<SNPCCBlob>(cc_blob.get());
			unsafe { p.as_ref() }
        })?;

        assert_eq!(size_of::<SNPSecretsPage>(), cc_blob.secrets_page_size as usize);
		let secrets = RwSpinLock::new(unsafe {
            cc_blob.secrets_page_pa.as_mut()?
        });

        assert_eq!(size_of::<CPUIDPage>(), cc_blob.cpuid_page_size as usize);
		let cpuid = unsafe {
            cc_blob.cpuid_page_pa.as_mut()?
        };

		Some(Self { secrets, cpuid })
    }

    pub fn secrets(&self) -> &SNPSecrets {
        &self.secrets
    }

    pub fn cpuid(&self) -> &SNPCpuIdValues {
        &self.cpuid
    }
}

impl SecretsPageAccessor for ConfidentialComputingBlob {
    fn with_secrets_page<F, R>(&self, func: F) -> R
    where
        F: Fn(&mut SNPSecretsPage) -> R
    {
        let mut lock = self.secrets().write();
        func(lock.deref_mut())
    }
}

impl CpuIdPageAccessor for ConfidentialComputingBlob {
    fn with_cpuid_page<F, R>(&self, func: F) -> R
    where
        F: FnOnce(&CPUIDPage) -> R
    {
        func(self.cpuid())
    }
}

type SNPSecrets = RwSpinLock<&'static mut SNPSecretsPage>;

type SNPCpuIdValues = &'static CPUIDPage;

#[derive(Debug)]
#[repr(C)]
struct SNPCCBlob {
    header: u32,
    version: u16,
    reserved1: u16,
    secrets_page_pa: *mut SNPSecretsPage,
    secrets_page_size: u32,
    reserved2: u32,
    cpuid_page_pa: *mut CPUIDPage,
    cpuid_page_size: u32,
    reserved3: u32,
}


pub(crate) static CC_BLOB: Lazy<ConfidentialComputingBlob> = Lazy::new(|| {
    ConfidentialComputingBlob::new().expect("missing AMD confidential computing blob - are you using a compatible bootloader?")
});

#[inline(always)]
pub fn cc_blob() -> &'static ConfidentialComputingBlob {
    Lazy::force(&CC_BLOB)
}