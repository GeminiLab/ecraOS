use core::marker::PhantomData;

use memory_addr::{MemoryAddr, PhysAddr};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{DynPagingHandler, PageTable, PageTableMeta, PagingHandler, PagingResult};

#[derive(Clone)]
struct PageTableMethods<A: MemoryAddr> {
    pub new_alloc: fn(handler: &DynPagingHandler) -> PagingResult<PhysAddr>,
    pub map: fn(
        handler: &DynPagingHandler,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult,
    pub unmap: fn(handler: &DynPagingHandler, vaddr: A, size: usize) -> PagingResult,
}

pub struct OpaquePageTableType<A: MemoryAddr> {
    methods: PageTableMethods<A>,
}

impl<A: MemoryAddr> OpaquePageTableType<A> {
    pub fn new<M, PTE: GenericPTE>() -> Self
    where
        M: PageTableMeta<VirtAddr = A>,
        [(); M::LEVELS - 1]: Sized,
    {
        Self {
            methods: PageTableMethods {
                new_alloc: |handler| PageTable::<M, PTE>::new_alloc_dyn(handler).map(|pt| pt.root),
                map: |handler, vaddr, paddr, size, flags| todo!(),
                unmap: |handler, vaddr, size| todo!(),
            },
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that the physical address is valid.
    pub unsafe fn new_pagetable_at(&self, paddr: PhysAddr) -> OpaquePageTable<A> {
        OpaquePageTable {
            root: paddr,
            methods: self.methods.clone(),
            _phantom: PhantomData,
        }
    }

    pub fn new_pagetable_alloc<H: PagingHandler>(&self) -> PagingResult<OpaquePageTable<A>> {
        unsafe {
            Ok(self.new_pagetable_at((self.methods.new_alloc)(&DynPagingHandler::new::<H>())?))
        }
    }
}

pub struct OpaquePageTable<A: MemoryAddr> {
    root: PhysAddr,
    methods: PageTableMethods<A>,
    _phantom: PhantomData<A>,
}

impl<A: MemoryAddr> OpaquePageTable<A> {
    pub fn root_paddr(&self) -> PhysAddr {
        self.root
    }

    pub fn map(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult {
        todo!()
    }

    pub fn unmap(&mut self, vaddr: A, size: usize) -> PagingResult {
        todo!()
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
