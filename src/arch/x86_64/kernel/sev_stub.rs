use x86_64::registers::model_specific::Msr;
use crate::arch::kernel::serial::early_panic;

pub fn ensure_sev_disabled() {
    // Check for SME support
    let (cpuid_max, _) = core::arch::x86_64::__get_cpuid_max(0x8000_0000);
    if cpuid_max < 0x8000_001F {
        return;
    }

    let sme_features = core::arch::x86_64::__cpuid(0x8000_001F);
    let sme_available = (sme_features.eax & 0x3) != 0; // either SME or SEV bit is set
    if !sme_available {
        return;
    }

    // Check the MSR to see if SME is currently enabled
    let sev_status = Msr::new(0xc0010131);
    if unsafe { sev_status.read() } == 0 {
        return;
    }

    early_panic("SEV/SEV-SNP appears to be enabled on guest, but current Hermit build does not have the `amd-sev` feature.\n\
    Recompile with `amd-sev` feature to enable.")
}