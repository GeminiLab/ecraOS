//! Slab allocator with per-CPU caches and lock-free cross-CPU freeing.
//!
//! The [`SlabAllocator`] is a standalone component that manages object allocation
//! within pre-supplied slab pages. It does **not** allocate pages itself; instead
//! it returns [`SlabAllocResult::NeedsSlab`] to request pages from the caller.
//!
//! Cross-CPU frees go through the lock-free [`SlabPageHeader::remote_free`] path.

use crate::{
    cache::{CacheDeallocResult, SlabCache},
    error::{AllocError, AllocResult},
    page::SlabPageHeader,
    size_class::SizeClass,
};

use core::alloc::Layout;
use core::ptr::NonNull;

use kspin::SpinNoIrq;
use memory_addr::VirtAddr;

/// Result of a slab allocation attempt.
pub enum SlabAllocResult {
    /// Object successfully allocated.
    Allocated(NonNull<u8>),
    /// The slab cache for this size class has no free objects.
    ///
    /// The caller should allocate `pages` pages from the buddy allocator,
    /// call [`SlabAllocator::add_slab`], and retry.
    NeedsSlab { size_class: SizeClass, pages: usize },
}

/// Result of a slab deallocation.
pub enum SlabDeallocResult {
    /// Object freed, nothing else to do.
    Done,
    /// The slab page at `base` became empty and should be returned to the buddy.
    FreeSlab { base: VirtAddr, pages: usize },
}

/// Result of a pool-mediated slab deallocation.
pub enum SlabPoolDeallocResult {
    /// Object freed on the local CPU path.
    Done,
    /// Object was queued onto the owner's remote-free list.
    RemoteQueued,
    /// The slab page at `base` became empty and should be returned to the buddy.
    FreeSlab { base: VirtAddr, pages: usize },
}

/// Object-safe slab interface used by integrators.
pub trait SlabTrait: Sync {
    /// Returns the logical CPU id this slab belongs to.
    fn cpu_id(&self) -> usize;

    /// Returns the page size used by this slab.
    fn page_size(&self) -> usize;

    /// Allocates one object.
    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult>;

    /// Registers a freshly allocated slab page.
    fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize);

    /// Frees an object on the owner CPU path.
    fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult;

    /// Frees an object on the remote CPU path.
    fn dealloc_remote(&self, ptr: NonNull<u8>) {
        let owner_cpu = u16::try_from(self.cpu_id()).expect("CPU id exceeds slab owner range");
        unsafe { SlabPageHeader::remote_free_object(ptr, owner_cpu, self.page_size()) };
    }
}

/// Object-safe slab-pool interface used by integrators.
pub trait SlabPoolTrait: Sync {
    /// Returns the slab belonging to the current CPU.
    fn current_slab(&self) -> &dyn SlabTrait;

    /// Returns the owner slab for the given CPU.
    fn owner_slab(&self, cpu_idx: usize) -> &dyn SlabTrait;

    /// Returns the logical CPU id of the current CPU.
    fn current_cpu_id(&self) -> usize {
        self.current_slab().cpu_id()
    }

    /// Allocates one object from the current CPU's slab.
    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        self.current_slab().alloc(layout)
    }

    /// Registers a freshly allocated slab page in the current CPU's slab.
    fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize) {
        self.current_slab().add_slab(size_class, base, bytes)
    }

    /// Frees an object, routing to local or remote slab ownership as needed.
    fn dealloc(&self, ptr: NonNull<u8>, layout: Layout, owner_cpu: usize) -> SlabPoolDeallocResult {
        if owner_cpu == self.current_cpu_id() {
            match self.current_slab().dealloc_local(ptr, layout) {
                SlabDeallocResult::Done => SlabPoolDeallocResult::Done,
                SlabDeallocResult::FreeSlab { base, pages } => {
                    SlabPoolDeallocResult::FreeSlab { base, pages }
                }
            }
        } else {
            self.owner_slab(owner_cpu).dealloc_remote(ptr);
            SlabPoolDeallocResult::RemoteQueued
        }
    }
}

/// Convenience helpers for callback-style slab access.
pub trait SlabPoolExt: SlabPoolTrait {
    /// Accesses the current CPU's slab via a callback.
    fn with_current_slab<R>(&self, f: impl FnOnce(&dyn SlabTrait) -> R) -> R {
        f(self.current_slab())
    }

    /// Accesses the given owner's slab via a callback.
    fn with_owner_slab<R>(&self, cpu_idx: usize, f: impl FnOnce(&dyn SlabTrait) -> R) -> R {
        f(self.owner_slab(cpu_idx))
    }
}

impl<T: ?Sized + SlabPoolTrait> SlabPoolExt for T {}

/// Standalone slab allocator (one per CPU or standalone use).
pub struct SlabAllocator {
    /// Per-size-class slab caches.
    caches: [SlabCache; SizeClass::COUNT],
    /// Page size used by this allocator.
    page_size: usize,
}

/// Default per-CPU slab wrapper used by integrators.
pub struct PerCpuSlab {
    /// Logical CPU id of the owner CPU.
    cpu_id: u16,
    /// Page size used by this allocator.
    page_size: usize,
    /// The inner slab allocator protected by a spinlock.
    inner: SpinNoIrq<SlabAllocator>,
}

/// Default static slab-pool wrapper used by integrators.
#[allow(dead_code)]
pub struct StaticSlabPool<const N: usize = 1> {
    /// Per-CPU slab wrappers.
    slabs: [PerCpuSlab; N],
    /// Function to determine the current CPU id.
    current_cpu_id: fn() -> usize,
    /// Page size used by this pool.
    page_size: usize,
}

impl PerCpuSlab {
    /// Creates an empty per-CPU slab wrapper for `cpu_id`.
    pub const fn new(cpu_id: u16, page_size: usize) -> Self {
        Self {
            cpu_id,
            page_size,
            inner: SpinNoIrq::new(SlabAllocator::new(page_size)),
        }
    }

    /// Resets the inner slab allocator to an empty state.
    pub fn reset(&self) {
        *self.inner.lock() = SlabAllocator::new(self.page_size);
    }

    /// Returns this slab's logical CPU id.
    pub const fn cpu_id(&self) -> usize {
        self.cpu_id as usize
    }

    /// Allocates one object.
    pub fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        self.inner.lock().alloc(layout)
    }

    /// Registers a freshly allocated slab page.
    pub fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize) {
        self.inner
            .lock()
            .add_slab(size_class, base, bytes, self.cpu_id);
    }

    /// Frees an object on the owner CPU path.
    pub fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        self.inner.lock().dealloc(ptr, layout)
    }

    /// Queues an object onto this slab's remote-free list.
    pub fn dealloc_remote(&self, ptr: NonNull<u8>) {
        unsafe { SlabPageHeader::remote_free_object(ptr, self.cpu_id, self.page_size) };
    }
}

impl<const N: usize> StaticSlabPool<N> {
    /// Creates a static slab pool from pre-built per-CPU slabs and a CPU-id hook.
    pub const fn new(
        slabs: [PerCpuSlab; N],
        current_cpu_id: fn() -> usize,
        page_size: usize,
    ) -> Self {
        Self {
            slabs,
            current_cpu_id,
            page_size,
        }
    }
}

impl SlabAllocator {
    /// Creates a new (empty) slab allocator for the given page size.
    ///
    /// No pages are owned yet.
    pub const fn new(page_size: usize) -> Self {
        Self {
            caches: [
                SlabCache::new(SizeClass::Bytes8),
                SlabCache::new(SizeClass::Bytes16),
                SlabCache::new(SizeClass::Bytes32),
                SlabCache::new(SizeClass::Bytes64),
                SlabCache::new(SizeClass::Bytes128),
                SlabCache::new(SizeClass::Bytes256),
                SlabCache::new(SizeClass::Bytes512),
                SlabCache::new(SizeClass::Bytes1024),
                SlabCache::new(SizeClass::Bytes2048),
            ],
            page_size,
        }
    }
}

impl Default for SlabAllocator {
    fn default() -> Self {
        Self::new(0x1000)
    }
}

impl SlabAllocator {
    /// Tries to allocate an object matching `layout`.
    ///
    /// If the matching cache is exhausted, [`SlabAllocResult::NeedsSlab`] is returned
    /// so the caller can supply pages and retry.
    pub fn alloc(&mut self, layout: Layout) -> AllocResult<SlabAllocResult> {
        let sc = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let cache = &mut self.caches[sc.index()];

        match cache.alloc_object(self.page_size) {
            Some(addr) => {
                // SAFETY: `addr` is non-null, aligned, and within a live slab page.
                let ptr = unsafe { NonNull::new_unchecked(addr as *mut u8) };
                Ok(SlabAllocResult::Allocated(ptr))
            }
            None => Ok(SlabAllocResult::NeedsSlab {
                size_class: sc,
                pages: sc.slab_pages(self.page_size),
            }),
        }
    }

    /// Frees an object previously allocated with [`alloc`](Self::alloc).
    ///
    /// This is the **local** (owner-CPU) path. Cross-CPU frees should go through
    /// [`SlabPageHeader::remote_free`] directly.
    pub fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        let sc = SizeClass::from_layout(layout).expect("layout exceeds slab size");
        let cache = &mut self.caches[sc.index()];

        match cache.dealloc_object(self.page_size, ptr.as_ptr() as usize) {
            CacheDeallocResult::Done => SlabDeallocResult::Done,
            CacheDeallocResult::FreeSlab { base, pages } => SlabDeallocResult::FreeSlab {
                base: VirtAddr::from(base),
                pages,
            },
        }
    }

    /// Supplies a freshly allocated slab page to the given size class.
    ///
    /// `base` is the virtual address of the page(s), `bytes` = pages * page_size.
    pub fn add_slab(&mut self, size_class: SizeClass, base: usize, bytes: usize, owner_cpu: u16) {
        self.caches[size_class.index()].add_slab(base, bytes, owner_cpu);
    }
}

impl SlabTrait for PerCpuSlab {
    fn cpu_id(&self) -> usize {
        PerCpuSlab::cpu_id(self)
    }

    fn page_size(&self) -> usize {
        self.page_size
    }

    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        PerCpuSlab::alloc(self, layout)
    }

    fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize) {
        PerCpuSlab::add_slab(self, size_class, base, bytes)
    }

    fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        PerCpuSlab::dealloc_local(self, ptr, layout)
    }
}

impl<const N: usize> SlabPoolTrait for StaticSlabPool<N> {
    fn current_slab(&self) -> &dyn SlabTrait {
        &self.slabs[(self.current_cpu_id)()]
    }

    fn owner_slab(&self, cpu_idx: usize) -> &dyn SlabTrait {
        &self.slabs[cpu_idx]
    }
}
