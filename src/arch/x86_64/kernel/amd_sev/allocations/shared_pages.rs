use ghcb::structures::snp_guest_request::shared_page::{SNPAllocatedSharedPage, SharedPageAccessor};
use hermit_sync::{InterruptSpinMutex, Lazy};
use crate::arch::kernel::amd_sev::mmap::SevAllocator;

type AllocatedSharedPage = SNPAllocatedSharedPage<SevAllocator>;

#[repr(transparent)]
pub(crate) struct RequestPageMutex(InterruptSpinMutex<AllocatedSharedPage>);

pub(crate) static REQUEST_PAGE: Lazy<RequestPageMutex> = Lazy::new(|| allocate_snp_shared_page());
pub(crate) static RESPONSE_PAGE: Lazy<RequestPageMutex> = Lazy::new(|| allocate_snp_shared_page());

fn allocate_snp_shared_page() -> RequestPageMutex {
	RequestPageMutex(InterruptSpinMutex::new(
		AllocatedSharedPage::allocate()
	))
}

impl SharedPageAccessor for RequestPageMutex {
	type Alloc = SevAllocator;

	fn with_shared_page<F, R>(&self, func: F) -> R
	where
		F: FnOnce(&mut AllocatedSharedPage) -> R,
	{
		let mut lock = self.0.lock();
		func(&mut *lock)
	}
}
