//! Slab headers, bitmaps, and remote publication using precomputed layouts.

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use memory_addr::{VirtAddr, va};

use crate::{SlabLayout, size_class::SizeClass};

/// Fixed byte offset of the first-level bitmap.
const BITMAP_OFFSET: usize = 64;
/// Byte offset of the reserved summary bitmap.
const SUMMARY_OFFSET: usize = 32;
/// Number of bytes reserved for summary words.
const SUMMARY_BYTES: usize = 32;
/// Number of slots covered by each summary bit.
const GROUP_SLOTS: usize = 512;
/// Number of first-level words in a summary group.
const GROUP_WORDS: usize = 8;

/// Common header for a slab using a precomputed geometry record.
///
/// The owner serializes access to the links and local free count. Remote producers access only
/// the atomic head and immutable fields. Concurrent operations must project individual fields
/// through raw pointers rather than create references covering the complete header.
#[repr(C)]
pub struct SlabPageHeader {
    /// Head of the remotely freed slot list, or zero when no frees are pending.
    pub remote_free_head: AtomicUsize,
    /// Previous slab in the owner-managed cache list.
    pub list_prev: Option<NonNull<SlabPageHeader>>,
    /// Next slab in the owner-managed cache list.
    pub list_next: Option<NonNull<SlabPageHeader>>,
    /// Immutable logical CPU id that owns the slab.
    pub owner_cpu: u16,
    /// Immutable slot capacity initialized from the compile-time layout.
    pub object_count: u16,
    /// Number of slots available in the owner-managed bitmap.
    pub local_free_count: u16,
    /// Immutable object size class.
    pub size_class: SizeClass,
}

impl SlabPageHeader {
    /// Publishes a freed slot to its owning slab.
    ///
    /// # Safety
    ///
    /// `this` must identify a live initialized slab header. `obj_addr` must identify a live slot
    /// in that slab, freed exactly once. The header and slot must stay live until publication.
    /// No reference covering the complete header may overlap this operation.
    pub unsafe fn remote_free(this: NonNull<Self>, obj_addr: VirtAddr) {
        // SAFETY: Only the atomic field is borrowed, leaving owner-managed fields unborrowed.
        let head = unsafe { &(*this.as_ptr()).remote_free_head };
        let mut previous = head.load(Ordering::Relaxed);
        loop {
            // SAFETY: A producer owns its freed slot until the successful release publication.
            unsafe { obj_addr.as_mut_ptr_of::<usize>().write(previous) };
            match head.compare_exchange_weak(
                previous,
                obj_addr.as_usize(),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                // The owner may reuse the slot or reclaim backing immediately after publication.
                Ok(_) => return,
                Err(current) => previous = current,
            }
        }
    }

    /// Returns whether a live slab has pending remote frees.
    ///
    /// # Safety
    ///
    /// `this` must remain a live initialized header throughout the atomic load.
    pub(crate) unsafe fn has_remote_frees(this: NonNull<Self>) -> bool {
        // SAFETY: The caller supplies a live atomic field and no owner fields are borrowed.
        unsafe { &(*this.as_ptr()).remote_free_head }.load(Ordering::Acquire) != 0
    }

    /// Detaches a published remote-free batch for the owner.
    ///
    /// # Safety
    ///
    /// `this` must be live and the caller must serialize owner-side operations.
    unsafe fn detach_remote_frees(this: NonNull<Self>) -> usize {
        // SAFETY: The field is atomic and its containing backing remains allocated.
        unsafe { &(*this.as_ptr()).remote_free_head }.swap(0, Ordering::Acquire)
    }
}

/// Header-only view of backing whose geometry is supplied as immutable data.
///
/// Memory operations use raw field projections so remote producers never borrow owner state.
#[repr(transparent)]
pub(crate) struct SlabPage(SlabPageHeader);

impl SlabPage {
    /// Initializes complete exclusive backing using a validated immutable geometry.
    ///
    /// # Safety
    ///
    /// The pointer must identify writable backing of the selected size, aligned to that size.
    /// No previous live slots, references, or remote operations may access the backing.
    pub(crate) unsafe fn init_at(this: *mut Self, layout: &SlabLayout, owner_cpu: u16) {
        assert!(core::mem::size_of::<SlabPageHeader>() == SUMMARY_OFFSET);
        assert!(!this.is_null());
        assert!(this.addr().is_multiple_of(layout.slab_size));
        let header = SlabPageHeader {
            remote_free_head: AtomicUsize::new(0),
            list_prev: None,
            list_next: None,
            owner_cpu,
            object_count: layout.slot_count as u16,
            local_free_count: layout.slot_count as u16,
            size_class: layout.size_class,
        };
        // SAFETY: The caller supplies exclusive complete backing, including both bitmap regions.
        unsafe {
            this.cast::<SlabPageHeader>().write(header);
            this.cast::<u8>()
                .add(SUMMARY_OFFSET)
                .write_bytes(0, SUMMARY_BYTES);
            let this = NonNull::new_unchecked(this);
            for word in 0..layout.bitmap_words {
                let remaining = layout.slot_count - word * 64;
                Self::bitmap_word(this, word).write(u64::MAX >> (64 - remaining.min(64)));
            }
            if layout.summary_words != 0 {
                for group in 0..layout.slot_count.div_ceil(GROUP_SLOTS) {
                    let summary = Self::summary_word(this, group / 64);
                    summary.write(summary.read() | (1 << (group % 64)));
                }
            }
        }
    }

    /// Projects a first-level word without borrowing complete backing.
    ///
    /// # Safety
    ///
    /// Backing must be live and complete. The word must be below the layout's bitmap length.
    unsafe fn bitmap_word(this: NonNull<Self>, word: usize) -> *mut u64 {
        // SAFETY: The caller provides complete backing and an in-bounds word index.
        unsafe {
            this.as_ptr()
                .cast::<u8>()
                .add(BITMAP_OFFSET)
                .cast::<u64>()
                .add(word)
        }
    }

    /// Projects a reserved summary word without borrowing owner-managed fields.
    ///
    /// # Safety
    ///
    /// Backing must be live and the word must be below the layout's active summary length.
    unsafe fn summary_word(this: NonNull<Self>, word: usize) -> *mut u64 {
        // SAFETY: The caller provides complete backing and an in-bounds summary index.
        unsafe {
            this.as_ptr()
                .cast::<u8>()
                .add(SUMMARY_OFFSET)
                .cast::<u64>()
                .add(word)
        }
    }

    /// Returns the first slot address for matching backing and geometry.
    fn slot_start(this: NonNull<Self>, layout: &SlabLayout) -> VirtAddr {
        VirtAddr::from_mut_ptr_of(this.as_ptr()) + layout.slot_offset
    }

    /// Returns a slot address after checking its capacity bound.
    fn slot_addr(this: NonNull<Self>, layout: &SlabLayout, index: usize) -> VirtAddr {
        assert!(index < layout.slot_count);
        Self::slot_start(this, layout) + (index << layout.object_log2)
    }

    /// Locates the header from a live object using the geometry's backing mask.
    ///
    /// # Safety
    ///
    /// The address must identify a live slot in a slab with this exact geometry.
    pub(crate) unsafe fn from_object(address: VirtAddr, layout: &SlabLayout) -> NonNull<Self> {
        let base = va!(address.as_usize() & layout.slab_mask);
        let this = NonNull::new(base.as_mut_ptr_of::<Self>()).expect("live slab base is nonzero");
        debug_assert!(address >= Self::slot_start(this, layout));
        debug_assert!(address < base + layout.slab_size);
        debug_assert!(
            (address - Self::slot_start(this, layout)).is_multiple_of(1 << layout.object_log2)
        );
        this
    }

    /// Takes a slot using the common first-level and optional summary algorithms.
    ///
    /// # Safety
    ///
    /// Backing must be live, initialized with this geometry, and locally owner-serialized.
    pub(crate) unsafe fn local_alloc(this: NonNull<Self>, layout: &SlabLayout) -> Option<VirtAddr> {
        let header = this.as_ptr().cast::<SlabPageHeader>();
        // SAFETY: The caller serializes this individual local field.
        if unsafe { (*header).local_free_count } == 0 {
            return None;
        }
        let (first, end) = if layout.summary_words != 0 {
            let group = (0..layout.summary_words)
                .find_map(|word| {
                    // SAFETY: The owner reads an initialized in-bounds summary word.
                    let bits = unsafe { Self::summary_word(this, word).read() };
                    (bits != 0).then(|| word * 64 + bits.trailing_zeros() as usize)
                })
                .expect("free count and summary must agree");
            let first = group * GROUP_WORDS;
            (first, (first + GROUP_WORDS).min(layout.bitmap_words))
        } else {
            (0, layout.bitmap_words)
        };
        for word in first..end {
            // SAFETY: The selected bitmap interval is initialized and within backing.
            let bitmap = unsafe { Self::bitmap_word(this, word) };
            let value = unsafe { bitmap.read() };
            if value == 0 {
                continue;
            }
            let bit = value.trailing_zeros() as usize;
            let remaining = value & (value - 1);
            // SAFETY: The owner serializes local count and bitmap updates.
            unsafe {
                bitmap.write(remaining);
                (*header).local_free_count -= 1;
                if layout.summary_words != 0
                    && remaining == 0
                    && (word + 1..end).all(|index| Self::bitmap_word(this, index).read() == 0)
                {
                    let group = word / GROUP_WORDS;
                    let summary = Self::summary_word(this, group / 64);
                    summary.write(summary.read() & !(1 << (group % 64)));
                }
            }
            return Some(Self::slot_addr(this, layout, word * 64 + bit));
        }
        unreachable!("free count and bitmap must agree")
    }

    /// Restores a live slot to its local bitmap and summary group.
    ///
    /// # Safety
    ///
    /// The slot must be live in backing of this geometry and freed once by the serialized owner.
    pub(crate) unsafe fn local_free(this: NonNull<Self>, layout: &SlabLayout, address: VirtAddr) {
        let offset = address - Self::slot_start(this, layout);
        debug_assert!(offset.is_multiple_of(1 << layout.object_log2));
        let index = offset >> layout.object_log2;
        debug_assert!(index < layout.slot_count);
        // SAFETY: The caller identifies a live slot and serializes bitmap and local count access.
        unsafe {
            let bitmap = Self::bitmap_word(this, index / 64);
            let mask = 1 << (index % 64);
            debug_assert_eq!(bitmap.read() & mask, 0, "double free");
            bitmap.write(bitmap.read() | mask);
            if layout.summary_words != 0 {
                let group = index / GROUP_SLOTS;
                let summary = Self::summary_word(this, group / 64);
                summary.write(summary.read() | (1 << (group % 64)));
            }
            let header = this.as_ptr().cast::<SlabPageHeader>();
            (*header).local_free_count += 1;
        }
    }

    /// Drains one remote batch with the same unconditional swap as the generic baselines.
    ///
    /// # Safety
    ///
    /// Backing must be initialized with this layout and owner-side operations serialized.
    pub(crate) unsafe fn drain_remote_frees(this: NonNull<Self>, layout: &SlabLayout) {
        // SAFETY: The caller supplies a live header and serializes the sole consumer.
        let mut head = unsafe { SlabPageHeader::detach_remote_frees(this.cast()) };
        while head != 0 {
            let address = va!(head);
            // SAFETY: Acquire detachment made this published link visible to its sole consumer.
            head = unsafe { address.as_ptr_of::<usize>().read() };
            unsafe { Self::local_free(this, layout, address) };
        }
    }
}
