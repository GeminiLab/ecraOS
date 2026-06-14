use expalloc_trait::{DynPageAllocator, PageAllocator};
use memory_addr::{MemoryAddr, PhysAddr};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{PageTable, PageTableMeta, PagingResult};

#[derive(Clone)]
struct PageTableMethods<A: MemoryAddr> {
    pub new_alloc: fn(handler: DynPageAllocator) -> PagingResult<PhysAddr>,
    pub map: fn(
        root: PhysAddr,
        handler: DynPageAllocator,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult,
    pub unmap: fn(root: PhysAddr, handler: DynPageAllocator, vaddr: A, size: usize) -> PagingResult,
}

impl<A: MemoryAddr> PageTableMethods<A> {
    pub const fn dummy() -> Self {
        fn panic_im_dummy() -> ! {
            panic!("dummy page table type does not support any actual operations")
        }

        Self {
            new_alloc: |_| panic_im_dummy(),
            map: |_, _, _, _, _, _| panic_im_dummy(),
            unmap: |_, _, _, _| panic_im_dummy(),
        }
    }
}

#[derive(Clone)]
pub struct OpaquePageTableType<A: MemoryAddr> {
    methods: PageTableMethods<A>,
}

impl<A: MemoryAddr> OpaquePageTableType<A> {
    pub const fn new<M, PTE: GenericPTE>() -> Self
    where
        M: PageTableMeta<VirtAddr = A>,
        [(); M::LEVELS - 1]: Sized,
    {
        Self {
            methods: PageTableMethods {
                new_alloc: |handler| {
                    PageTable::<M, PTE>::new_alloc_dyn(handler).map(|pt: PageTable<M, PTE>| pt.root)
                },
                map: |root, handler, vaddr, paddr, size, flags| {
                    let mut pt = unsafe { PageTable::<M, PTE>::new_at(root) };
                    pt.cursor().map_dyn(vaddr, paddr, size, flags, handler)
                },
                unmap: |root, handler, vaddr, size| {
                    let mut pt = unsafe { PageTable::<M, PTE>::new_at(root) };
                    pt.cursor().unmap_dyn(vaddr, size, handler)
                },
            },
        }
    }

    pub const fn dummy() -> Self {
        Self {
            methods: PageTableMethods::dummy(),
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that the physical address is valid.
    pub unsafe fn new_pagetable_at(&self, paddr: PhysAddr) -> OpaquePageTable<A> {
        OpaquePageTable {
            root: paddr,
            methods: self.methods.clone(),
        }
    }

    pub fn new_pagetable_alloc<H: PageAllocator>(&self) -> PagingResult<OpaquePageTable<A>> {
        unsafe {
            Ok(self.new_pagetable_at((self.methods.new_alloc)(DynPageAllocator::new::<H>())?))
        }
    }
}

pub struct OpaquePageTable<A: MemoryAddr> {
    root: PhysAddr,
    methods: PageTableMethods<A>,
}

impl<A: MemoryAddr> OpaquePageTable<A> {
    pub const fn dummy() -> Self {
        Self {
            root: PhysAddr::from_usize(0),
            methods: PageTableMethods::dummy(),
        }
    }

    pub fn root_paddr(&self) -> PhysAddr {
        self.root
    }

    pub fn map<H: PageAllocator>(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult {
        (self.methods.map)(
            self.root,
            DynPageAllocator::new::<H>(),
            vaddr,
            paddr,
            size,
            flags,
        )
    }

    pub fn unmap<H: PageAllocator>(&mut self, vaddr: A, size: usize) -> PagingResult {
        (self.methods.unmap)(self.root, DynPageAllocator::new::<H>(), vaddr, size)
    }

    /// This method is not implemented yet.
    ///
    /// TODO: Support cursor operations on opaque page tables.
    ///
    /// The main reason why cursor is not supported now is that the opaque page
    /// table types does not really store instances of the underlying, concrete
    /// page table type, which means a reference to the page table cannot be
    /// obtained. In addition, the opaque types does not know the size of the
    /// underlying types.
    #[cfg(false)]
    pub fn cursor(&mut self) -> ! {
        todo!()
    }
}
