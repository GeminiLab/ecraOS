//! Slab page layout and the operations performed on one slab.
//!
//! This module is responsible for the in-slab side of allocation. It initializes slabs, tracks
//! locally available slots in bitmaps, and receives slots released by other CPUs.
//!
//! # Layout
//!
//! A slab occupies [`SlabLayout::slab_size`] bytes and starts at an address aligned to that size.
//! Byte ranges below are offsets from the slab base, `C` is the size-class slot size, and `N` is
//! [`SlabLayout::slot_count`].
//!
//! | Byte range | Contents |
//! | --- | --- |
//! | `0..32` | [`SlabPageHeader`] |
//! | `32..64` | Reserved summary bitmap |
//! | `64..64 + ceil(N / 64) * 8` | First-level bitmap stored as `u64` words |
//! | End of first-level bitmap through `slot_offset` | Padding for object alignment |
//! | `slot_offset..slab_size` | `N` object slots, each `C` bytes and aligned to `C` |
//!
//! [`SlabLayout::slot_offset`] is the smallest byte offset after the bitmap that is a multiple of
//! `C`. There is no trailing padding because the slot's size and alignment is always a multiple of
//! `C`.
//!
//! # Bitmaps
//!
//! Each bit in the first-level bitmap records whether its corresponding slot is locally available.
//! A set bit means available. Bits beyond `N` remain clear. Allocation clears the bit of the slot
//! it returns and local release sets it again, so a double-free triggers a debug assertion.
//!
//! A slab with more than 512 slots uses the summary bitmap as a second level. One summary bit marks
//! a group of 512 slots, and a set bit means the group has a locally available slot.  Allocation
//! locates a non-empty group through the summary and only scans the first-level bitmap of that
//! group. For `N <= 512` the region stays reserved and unused, and the first-level bitmap is
//! scanned directly. The region is a fixed 32 bytes region after the header, so the maximum number
//! of slots supported in a slab is `32 * 8 * 512 = 131072` which is far more than `object_count`'s
//! limit and any slab actually needs.
//!
//! Summary and first-level bitmaps are managed in 64-bit words.
//!
//! # Remote releases
//!
//! A slot released by another CPU is not recorded in the bitmap directly. The freeing CPU links it
//! into [`SlabPageHeader::remote_free_head`] with a compare-and-exchange loop, and the owning cache
//! later detaches whole batches and returns those slots to the bitmap. The header therefore carries
//! two kinds of state: fields only the owner touches, and the atomic head that remote CPUs update.
//! [`SlabPageHeader`] documents which is which.

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use memory_addr::{VirtAddr, va};

use crate::{
    SlabLayout,
    layout::{
        BITMAP_WORDS_PER_SUMMARY_GROUP, BITS_PER_BITMAP_WORD, FIRST_LEVEL_BITMAP_OFFSET,
        SLAB_HEADER_SIZE, SUMMARY_BITMAP_OFFSET, SUMMARY_BITMAP_SIZE, SUMMARY_GROUP_SLOT_COUNT,
    },
    size_class::SizeClass,
};

/// A slab page header.
///
/// The header stores slab ownership, intrusive-list links, local allocation state, and the atomic
/// head of the remote-free list. It is the only part of a slab with a fixed Rust type. The bitmaps
/// and object slots follow it in the same slab memory and are accessed through offsets.
///
/// # Concurrency
///
/// The slab's owner is the allocator or cache that registered it. Only owner operations may read or
/// write `list_prev`, `list_next`, and `local_free_count`, and the caller must prevent two owner
/// operations from running at once. Calls from tasks and from interrupt handlers on the same CPU
/// count as concurrent here, and this type provides no lock for those fields.
///
/// A remote free may run at the same time as owner operations. It uses only `remote_free_head` of
/// the header and the slot being released, and should not touch the owner-managed fields or the
/// bitmaps. `owner_cpu`, `object_count`, and `size_class` never change while the slab is alive, so
/// any CPU may read them safely.
///
/// Code that reaches slab memory through a raw pointer must borrow the individual fields it needs.
/// Writing `&(*ptr).remote_free_head`, for example, borrows only the atomic field. Forming a
/// reference to the whole header instead would claim that every other field stays unchanged for the
/// reference's lifetime, which conflicts with concurrent remote frees.
#[repr(C)]
pub struct SlabPageHeader {
    /// Head of the remotely freed slot list, or zero when no frees are pending.
    ///
    /// Updated concurrently by freeing CPUs and read by the owner. Each released slot stores the
    /// previous head value in its first machine word, forming the list.
    pub remote_free_head: AtomicUsize,
    /// Previous slab in the slab list. Used by the owner to iterate the list.
    pub list_prev: Option<NonNull<SlabPageHeader>>,
    /// Next slab in the slab list. Used by the owner to iterate the list.
    pub list_next: Option<NonNull<SlabPageHeader>>,
    /// Immutable logical CPU id that owns the slab.
    pub owner_cpu: u16,
    /// Immutable slot capacity taken from the bound layout.
    pub object_count: u16,
    /// Number of slots available in the owner-managed bitmap, excluding pending remote frees.
    pub local_free_count: u16,
    /// Immutable object size class.
    pub size_class: SizeClass,
}

/// Remote-free publication and reclamation for [`SlabPageHeader`].
impl SlabPageHeader {
    /// Publishes a freed slot to its owning slab.
    ///
    /// Runs on the freeing CPU without borrowing the owner. The slot's first word receives the
    /// current list head, and a successful `compare_exchange_weak` with [`Ordering::Release`] makes
    /// the slot and that link visible to the owner. After publication the slot belongs to the
    /// owner, which may reuse it or release the slab memory at any time.
    ///
    /// # Safety
    ///
    /// `this` must identify a live initialized slab header. `obj_addr` must identify a live slot
    /// in that slab, freed exactly once. The header and slot must stay live until publication.
    /// No reference covering the complete header may overlap this operation.
    pub unsafe fn remote_free(this: NonNull<Self>, obj_addr: VirtAddr) {
        // SAFETY: This reference covers only `remote_free_head`, not the complete header.
        // The owner accesses other fields individually through raw pointers.
        let head = unsafe { &(*this.as_ptr()).remote_free_head };
        let mut previous = head.load(Ordering::Relaxed);
        loop {
            // SAFETY: The caller owns the released slot until publication succeeds.
            unsafe { obj_addr.as_mut_ptr_of::<usize>().write(previous) };
            match head.compare_exchange_weak(
                previous,
                obj_addr.as_usize(),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                // Publication lets the owner immediately reuse the slot or reclaim slab memory.
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
        // SAFETY: This reference covers only the live `remote_free_head` field. The owner
        // accesses other fields individually through raw pointers.
        unsafe { &(*this.as_ptr()).remote_free_head }.load(Ordering::Acquire) != 0
    }

    /// Detaches a published remote-free batch for the owner.
    ///
    /// Uses [`Ordering::Acquire`] so the caller can read the links written before publication.
    ///
    /// # Safety
    ///
    /// `this` must be live and the caller must prevent concurrent local allocations and frees.
    unsafe fn detach_remote_frees(this: NonNull<Self>) -> usize {
        // SAFETY: This reference covers only the live atomic field. The slab memory
        // containing it remains allocated while the owner detaches the batch.
        unsafe { &(*this.as_ptr()).remote_free_head }.swap(0, Ordering::Acquire)
    }
}

/// A slab page used for object allocation.
///
/// The type covers only the header at the start of a slab. Its methods reach the bitmaps and object
/// slots that follow it in the same memory, locating them through the bound [`SlabLayout`]. The
/// caller owns the complete region and passes it in already aligned and writable, as described in
/// the module documentation.
#[repr(transparent)]
pub(crate) struct SlabPage(SlabPageHeader);

/// Local allocation and release operations for [`SlabPage`].
impl SlabPage {
    /// Initializes a slab page so that every slot is locally available.
    ///
    /// Writes the header from `layout`, clears the summary region, sets one first-level bit per
    /// slot, and marks the groups that have a free slot. Bits beyond the slot count stay clear.
    ///
    /// # Safety
    ///
    /// The pointer must identify writable slab memory of the selected size, aligned to that size.
    /// No previous live slots, references, or remote operations may access this memory.
    pub(crate) unsafe fn init_at(this: NonNull<Self>, layout: &SlabLayout, owner_cpu: u16) {
        assert!(core::mem::size_of::<SlabPageHeader>() == SLAB_HEADER_SIZE);
        assert!(this.as_ptr().addr().is_multiple_of(layout.slab_size));
        let header = SlabPageHeader {
            remote_free_head: AtomicUsize::new(0),
            list_prev: None,
            list_next: None,
            owner_cpu,
            object_count: layout.slot_count as u16,
            local_free_count: layout.slot_count as u16,
            size_class: layout.size_class,
        };
        // SAFETY: The caller supplies the complete slab memory, including both bitmap regions,
        // for exclusive use.
        unsafe {
            this.cast::<SlabPageHeader>().as_ptr().write(header);
            this.cast::<u8>()
                .add(SUMMARY_BITMAP_OFFSET)
                .write_bytes(0, SUMMARY_BITMAP_SIZE);
            for word in 0..layout.bitmap_words {
                let remaining = layout.slot_count - word * BITS_PER_BITMAP_WORD;
                Self::bitmap_word(this, word).write(
                    u64::MAX >> (BITS_PER_BITMAP_WORD - remaining.min(BITS_PER_BITMAP_WORD)),
                );
            }
            if layout.summary_words != 0 {
                for group in 0..layout.slot_count.div_ceil(SUMMARY_GROUP_SLOT_COUNT) {
                    let summary = Self::summary_word(this, group / BITS_PER_BITMAP_WORD);
                    summary.write(summary.read() | (1u64 << (group % BITS_PER_BITMAP_WORD)));
                }
            }
        }
    }

    /// Locates a first-level bitmap word.
    ///
    /// # Safety
    ///
    /// The slab memory must be valid and complete. The word must be below the layout's bitmap
    /// length.
    unsafe fn bitmap_word(this: NonNull<Self>, word: usize) -> *mut u64 {
        // SAFETY: The caller provides complete slab memory and an in-bounds word index.
        unsafe {
            this.as_ptr()
                .cast::<u8>()
                .add(FIRST_LEVEL_BITMAP_OFFSET)
                .cast::<u64>()
                .add(word)
        }
    }

    /// Locates a summary bitmap word.
    ///
    /// # Safety
    ///
    /// The slab memory must be valid and the word must be below the layout's active summary length.
    unsafe fn summary_word(this: NonNull<Self>, word: usize) -> *mut u64 {
        // SAFETY: The caller provides complete slab memory and an in-bounds summary index.
        unsafe {
            this.as_ptr()
                .cast::<u8>()
                .add(SUMMARY_BITMAP_OFFSET)
                .cast::<u64>()
                .add(word)
        }
    }

    /// Returns the first slot address for slab memory with a matching slab layout.
    fn slot_start(this: NonNull<Self>, layout: &SlabLayout) -> VirtAddr {
        VirtAddr::from_mut_ptr_of(this.as_ptr()) + layout.slot_offset
    }

    /// Returns the address of a slot.
    ///
    /// Panics if `index` is outside the layout's slot capacity.
    fn slot_addr(this: NonNull<Self>, layout: &SlabLayout, index: usize) -> VirtAddr {
        assert!(index < layout.slot_count);
        Self::slot_start(this, layout) + (index << layout.object_log2)
    }

    /// Locates the slab page containing a live object.
    ///
    /// Clears the low address bits, which identifies the base of a naturally aligned slab, rather
    /// than scanning for a marker. The caller must name the slab's layout, because the slab
    /// size in it determines how many bits to clear and nothing can be read before the base is
    /// known.
    ///
    /// # Safety
    ///
    /// The address must identify a live slot in a slab with this exact slab layout.
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

    /// Allocates one locally available slot.
    ///
    /// Returns `None` when the local count is zero. Otherwise clears and returns the first set
    /// first-level bit. When the layout uses a summary bitmap, the search covers only the first
    /// group with a set summary bit, and that bit is cleared once the group has no available slot.
    ///
    /// # Safety
    ///
    /// The slab memory must be valid and initialized with this slab layout. The caller must
    /// prevent concurrent local allocations and frees.
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
                    (bits != 0)
                        .then(|| word * BITS_PER_BITMAP_WORD + bits.trailing_zeros() as usize)
                })
                .expect("free count and summary must agree");
            let first = group * BITMAP_WORDS_PER_SUMMARY_GROUP;
            (
                first,
                (first + BITMAP_WORDS_PER_SUMMARY_GROUP).min(layout.bitmap_words),
            )
        } else {
            (0, layout.bitmap_words)
        };
        for word in first..end {
            // SAFETY: The selected bitmap interval is initialized and within the slab memory.
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
                    let group = word / BITMAP_WORDS_PER_SUMMARY_GROUP;
                    let summary = Self::summary_word(this, group / BITS_PER_BITMAP_WORD);
                    summary.write(summary.read() & !(1u64 << (group % BITS_PER_BITMAP_WORD)));
                }
            }
            return Some(Self::slot_addr(
                this,
                layout,
                word * BITS_PER_BITMAP_WORD + bit,
            ));
        }
        unreachable!("free count and bitmap must agree")
    }

    /// Releases one slot to the local allocator.
    ///
    /// Sets the slot's bitmap bit, marks its summary group, and increments the local count. A slot
    /// released twice trips a debug assertion.
    ///
    /// # Safety
    ///
    /// The slot must be live in slab memory with this slab layout. The slot must not have been
    /// released. The caller must prevent concurrent local allocations and frees.
    pub(crate) unsafe fn local_free(this: NonNull<Self>, layout: &SlabLayout, address: VirtAddr) {
        let offset = address - Self::slot_start(this, layout);
        debug_assert!(offset.is_multiple_of(1 << layout.object_log2));
        let index = offset >> layout.object_log2;
        debug_assert!(index < layout.slot_count);
        // SAFETY: The caller identifies a live slot and serializes bitmap and local count access.
        unsafe {
            let bitmap = Self::bitmap_word(this, index / BITS_PER_BITMAP_WORD);
            let mask = 1u64 << (index % BITS_PER_BITMAP_WORD);
            debug_assert_eq!(bitmap.read() & mask, 0, "double free");
            bitmap.write(bitmap.read() | mask);
            if layout.summary_words != 0 {
                let group = index / SUMMARY_GROUP_SLOT_COUNT;
                let summary = Self::summary_word(this, group / BITS_PER_BITMAP_WORD);
                summary.write(summary.read() | (1u64 << (group % BITS_PER_BITMAP_WORD)));
            }
            let header = this.as_ptr().cast::<SlabPageHeader>();
            (*header).local_free_count += 1;
        }
    }

    /// Drains one batch of remotely freed slots.
    ///
    /// The owner takes the whole published list with one atomic swap and walks it, returning each
    /// slot to the bitmap. The swap uses [`Ordering::Acquire`], which makes the links the freeing
    /// CPUs wrote visible. A release published after the swap stays on the head for a later drain.
    ///
    /// # Safety
    ///
    /// The slab memory must be initialized with this layout. The caller must prevent concurrent
    /// local allocations and frees.
    pub(crate) unsafe fn drain_remote_frees(this: NonNull<Self>, layout: &SlabLayout) {
        // SAFETY: The caller supplies a live header and serializes the sole consumer.
        let mut head = unsafe { SlabPageHeader::detach_remote_frees(this.cast()) };
        while head != 0 {
            let address = va!(head);
            // SAFETY: The swap with Ordering::Acquire made this link visible to the sole consumer.
            head = unsafe { address.as_ptr_of::<usize>().read() };
            unsafe { Self::local_free(this, layout, address) };
        }
    }
}
