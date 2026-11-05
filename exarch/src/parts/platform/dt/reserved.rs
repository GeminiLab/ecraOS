//! FDT memory reservation discovery.

use fdt_rs::{
    base::{DevTree, parse::ParsedTok},
    error::DevTreeError,
    prelude::FallibleIterator,
};
use heapless::Vec as HeaplessVec;
use memory_addr::{PhysAddrRange, pa};

use crate::mem::{MAX_RAW_MEM_REGIONS, MemoryRegion, MemoryRegionFlags, RawMemoryRegions};

/// The description attached to regions reserved by a device tree.
const DEVICE_TREE_RESERVED_DESC: &str = "device-tree reserved memory";

/// A statically located range declared by a device tree reservation mechanism.
#[derive(Clone, Copy)]
struct DeviceTreeReservedRegion {
    /// The physical range that firmware reserves from general use.
    range: PhysAddrRange,
    /// Whether the standard direct mapping must omit this range.
    no_map: bool,
}

/// A direct child of the device-tree reserved-memory node while it is being parsed.
struct ReservedMemoryChild<'dt> {
    /// The structure-token depth of this child node.
    depth: usize,
    /// The child reg property, if it describes a static physical range.
    reg: Option<&'dt [u8]>,
    /// Whether the child has a no-map property.
    no_map: bool,
    /// Whether the child has a reusable property.
    reusable: bool,
    /// Whether firmware marks the child enabled.
    enabled: bool,
}

/// Records all static FDT memory reservations in the physical region table.
///
/// Reads the FDT reservation map and static reg properties below /reserved-memory. Complete
/// ranges are recorded, including gaps in discovered RAM. Existing access and device attributes
/// are retained. Gaps receive only a RESERVED or NO_MAP role. A /reserved-memory child with no-map
/// becomes a NO_MAP region so both early aliases omit it. Dynamic size requests are not allocated
/// here because this function runs before the kernel allocator is available.
///
/// # Errors
///
/// Returns an error when the reservation map, structure block, reserved-memory properties, or
/// region capacity is invalid. All failures leave the input region table unchanged.
pub fn apply_device_tree_reservations(
    dev_tree: DevTree<'_>,
    regions: &mut RawMemoryRegions,
) -> Result<(), DevTreeError> {
    let mut reserved = HeaplessVec::<DeviceTreeReservedRegion, MAX_RAW_MEM_REGIONS>::new();

    // Parse both reservation sources before changing the discovered memory regions.
    parse_reservation_map(dev_tree, &mut reserved)?;
    parse_reserved_memory_nodes(dev_tree, &mut reserved)?;

    // Keep all input regions intact if any reservation exceeds the fixed storage capacity.
    let mut adjusted = regions.clone();
    for reservation in reserved {
        apply_reserved_region(&mut adjusted, reservation)?;
    }
    *regions = adjusted;

    Ok(())
}

/// Reads entries from the FDT header's memory-reservation block.
fn parse_reservation_map(
    dev_tree: DevTree<'_>,
    reserved: &mut HeaplessVec<DeviceTreeReservedRegion, MAX_RAW_MEM_REGIONS>,
) -> Result<(), DevTreeError> {
    let bytes = dev_tree.buf();
    let mut offset = dev_tree.off_mem_rsvmap();
    let end = dev_tree.off_dt_struct();

    // The reservation map must be aligned and fit before the structure block begins.
    if !offset.is_multiple_of(core::mem::align_of::<u64>()) || offset > end {
        return Err(DevTreeError::InvalidOffset);
    }

    loop {
        // Each entry contains two big-endian u64 values; (0, 0) terminates the map.
        let entry_end = offset
            .checked_add(2 * core::mem::size_of::<u64>())
            .ok_or(DevTreeError::InvalidOffset)?;
        let entry = bytes
            .get(offset..entry_end)
            .filter(|_| entry_end <= end)
            .ok_or(DevTreeError::InvalidOffset)?;
        let address = u64::from_be_bytes(
            entry[..8]
                .try_into()
                .map_err(|_| DevTreeError::ParseError)?,
        );
        let size = u64::from_be_bytes(
            entry[8..]
                .try_into()
                .map_err(|_| DevTreeError::ParseError)?,
        );
        if address == 0 && size == 0 {
            return Ok(());
        }
        if size == 0 {
            return Err(DevTreeError::ParseError);
        }

        reserved
            .push(DeviceTreeReservedRegion {
                range: physical_range(address, size)?,
                no_map: false,
            })
            .map_err(|_| DevTreeError::NotEnoughMemory)?;
        offset = entry_end;
    }
}

/// Reads static reg ranges from direct children of the /reserved-memory node.
fn parse_reserved_memory_nodes(
    dev_tree: DevTree<'_>,
    reserved: &mut HeaplessVec<DeviceTreeReservedRegion, MAX_RAW_MEM_REGIONS>,
) -> Result<(), DevTreeError> {
    let mut tokens = dev_tree.parse_iter();
    let mut depth = 0usize;
    let mut reserved_node_depth = None;
    let mut address_cells = None;
    let mut size_cells = None;
    let mut ranges_empty = None;
    let mut child: Option<ReservedMemoryChild<'_>> = None;

    // Stream the structure once. Depth tracking identifies the reserved-memory node and its
    // direct children without building an allocator-backed index.
    while let Some(token) = tokens.next()? {
        match token {
            ParsedTok::BeginNode(node) => {
                if depth == 1 && node.name == b"reserved-memory" {
                    reserved_node_depth = Some(depth);
                    address_cells = None;
                    size_cells = None;
                    ranges_empty = None;
                } else if depth
                    .checked_sub(1)
                    .is_some_and(|parent_depth| reserved_node_depth == Some(parent_depth))
                {
                    child = Some(ReservedMemoryChild {
                        depth,
                        reg: None,
                        no_map: false,
                        reusable: false,
                        enabled: true,
                    });
                }
                depth = depth.checked_add(1).ok_or(DevTreeError::ParseError)?;
            }
            ParsedTok::Prop(property) => {
                let owner_depth = depth.checked_sub(1).ok_or(DevTreeError::ParseError)?;
                let name = property_name(dev_tree, property.name_offset)?;

                // Cell counts and ranges belong to the parent; reg and policy properties belong
                // only to its direct reservation children.
                if reserved_node_depth == Some(owner_depth) {
                    match name {
                        "#address-cells" => {
                            address_cells = Some(read_cell_count(property.prop_buf)?);
                        }
                        "#size-cells" => {
                            size_cells = Some(read_cell_count(property.prop_buf)?);
                        }
                        "ranges" => ranges_empty = Some(property.prop_buf.is_empty()),
                        _ => {}
                    }
                } else if let Some(child) = child.as_mut()
                    && child.depth == owner_depth
                {
                    match name {
                        "reg" => child.reg = Some(property.prop_buf),
                        "no-map" if property.prop_buf.is_empty() => child.no_map = true,
                        "no-map" => return Err(DevTreeError::ParseError),
                        "reusable" if property.prop_buf.is_empty() => child.reusable = true,
                        "reusable" => return Err(DevTreeError::ParseError),
                        "status" => child.enabled = status_is_enabled(property.prop_buf)?,
                        _ => {}
                    }
                }
            }
            ParsedTok::EndNode => {
                let ended_depth = depth.checked_sub(1).ok_or(DevTreeError::ParseError)?;

                // Defer decoding reg until the child closes so its properties can appear in any
                // valid order.
                if child
                    .as_ref()
                    .is_some_and(|child| child.depth == ended_depth)
                {
                    let child = child.take().ok_or(DevTreeError::ParseError)?;
                    // The DT specification makes no-map and reusable mutually exclusive.
                    if child.no_map && child.reusable {
                        return Err(DevTreeError::ParseError);
                    }
                    if child.enabled
                        && let Some(reg) = child.reg
                    {
                        let address_cells = address_cells.ok_or(DevTreeError::InvalidParameter(
                            "reserved-memory is missing #address-cells",
                        ))?;
                        let size_cells = size_cells.ok_or(DevTreeError::InvalidParameter(
                            "reserved-memory is missing #size-cells",
                        ))?;
                        append_reg_ranges(reg, address_cells, size_cells, child.no_map, reserved)?;
                    }
                }
                if reserved_node_depth == Some(ended_depth) {
                    if ranges_empty != Some(true) {
                        return Err(DevTreeError::InvalidParameter(
                            "reserved-memory must have an empty ranges property",
                        ));
                    }
                    reserved_node_depth = None;
                }
                depth = ended_depth;
            }
            ParsedTok::Nop => {}
        }
    }

    Ok(())
}

/// Checks whether a null-terminated status property enables its node.
fn status_is_enabled(bytes: &[u8]) -> Result<bool, DevTreeError> {
    let Some(value) = bytes.strip_suffix(&[0]) else {
        return Err(DevTreeError::ParseError);
    };
    let value = core::str::from_utf8(value).map_err(DevTreeError::from)?;
    Ok(matches!(value, "okay" | "ok"))
}

/// Returns a property name from the FDT strings block.
fn property_name<'dt>(dev_tree: DevTree<'dt>, offset: usize) -> Result<&'dt str, DevTreeError> {
    let strings_start = dev_tree.off_dt_strings();
    let strings_end = strings_start
        .checked_add(dev_tree.size_dt_strings() as usize)
        .ok_or(DevTreeError::InvalidOffset)?;
    let strings = dev_tree
        .buf()
        .get(strings_start..strings_end)
        .ok_or(DevTreeError::InvalidOffset)?;
    let suffix = strings.get(offset..).ok_or(DevTreeError::InvalidOffset)?;
    let length = suffix
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(DevTreeError::ParseError)?;
    core::str::from_utf8(&suffix[..length]).map_err(DevTreeError::from)
}

/// Decodes a one- or two-cell count from a reserved-memory property.
fn read_cell_count(bytes: &[u8]) -> Result<usize, DevTreeError> {
    let [a, b, c, d] = bytes else {
        return Err(DevTreeError::ParseError);
    };
    let count = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
    if !(1..=2).contains(&count) {
        return Err(DevTreeError::InvalidParameter(
            "reserved-memory cell counts must be one or two",
        ));
    }
    Ok(count)
}

/// Decodes static physical ranges from a reserved-memory reg property.
fn append_reg_ranges(
    reg: &[u8],
    address_cells: usize,
    size_cells: usize,
    no_map: bool,
    reserved: &mut HeaplessVec<DeviceTreeReservedRegion, MAX_RAW_MEM_REGIONS>,
) -> Result<(), DevTreeError> {
    // A reg value is a sequence of address/size tuples sized by the parent cell counts.
    let tuple_size = (address_cells + size_cells)
        .checked_mul(core::mem::size_of::<u32>())
        .ok_or(DevTreeError::InvalidOffset)?;
    if reg.is_empty() || !reg.len().is_multiple_of(tuple_size) {
        return Err(DevTreeError::ParseError);
    }

    for tuple in reg.chunks_exact(tuple_size) {
        let address_size = address_cells * core::mem::size_of::<u32>();
        let address = decode_cells(&tuple[..address_size], address_cells)?;
        let size = decode_cells(&tuple[address_size..], size_cells)?;
        if size == 0 {
            return Err(DevTreeError::ParseError);
        }
        reserved
            .push(DeviceTreeReservedRegion {
                range: physical_range(address, size)?,
                no_map,
            })
            .map_err(|_| DevTreeError::NotEnoughMemory)?;
    }

    Ok(())
}

/// Decodes a one- or two-cell big-endian integer.
fn decode_cells(bytes: &[u8], count: usize) -> Result<u64, DevTreeError> {
    match count {
        1 => Ok(u32::from_be_bytes(bytes.try_into().map_err(|_| DevTreeError::ParseError)?) as u64),
        2 => Ok(u64::from_be_bytes(
            bytes.try_into().map_err(|_| DevTreeError::ParseError)?,
        )),
        _ => Err(DevTreeError::InvalidParameter(
            "reserved-memory cell counts must be one or two",
        )),
    }
}

/// Creates a semantic physical range and rejects address-space overflow.
fn physical_range(address: u64, size: u64) -> Result<PhysAddrRange, DevTreeError> {
    let end = address
        .checked_add(size)
        .ok_or(DevTreeError::InvalidOffset)?;
    let start = usize::try_from(address).map_err(|_| DevTreeError::InvalidOffset)?;
    let end = usize::try_from(end).map_err(|_| DevTreeError::InvalidOffset)?;
    Ok(PhysAddrRange::new(pa!(start), pa!(end)))
}

/// Records a complete reservation while preserving existing access and device attributes.
fn apply_reserved_region(
    regions: &mut RawMemoryRegions,
    reservation: DeviceTreeReservedRegion,
) -> Result<(), DevTreeError> {
    // Preserve evidence of required mappings before replacing any role with NO_MAP. Otherwise
    // the page planner would only see a barrier and silently omit boot data or the console.
    let required = MemoryRegionFlags::BOOT_SERVICE
        | MemoryRegionFlags::KERNEL
        | MemoryRegionFlags::DEVICE
        | MemoryRegionFlags::EXECUTE;
    if reservation.no_map
        && regions.overlaps(reservation.range).any(|old| {
            !old.flags.contains(MemoryRegionFlags::NO_MAP) && old.flags.intersects(required)
        })
    {
        return Err(DevTreeError::InvalidParameter(
            "no-map reservation overlaps a required early mapping",
        ));
    }
    let role = if reservation.no_map {
        MemoryRegionFlags::NO_MAP
    } else {
        MemoryRegionFlags::RESERVED
    };
    regions
        .overlay_with(
            MemoryRegion {
                range: reservation.range,
                // Gaps have no known access or device attributes. Record ownership only.
                flags: role,
                desc: DEVICE_TREE_RESERVED_DESC,
            },
            |old| {
                // An ordinary reservation cannot weaken an existing no-map declaration.
                if old.flags.contains(MemoryRegionFlags::NO_MAP)
                    || !reservation.no_map && !old.flags.contains(MemoryRegionFlags::FREE)
                {
                    return old;
                }
                let roles = MemoryRegionFlags::FREE
                    | MemoryRegionFlags::RESERVED
                    | MemoryRegionFlags::NO_MAP
                    | MemoryRegionFlags::BOOT_SERVICE
                    | MemoryRegionFlags::KERNEL;
                MemoryRegion {
                    flags: (old.flags & !roles) | role,
                    desc: DEVICE_TREE_RESERVED_DESC,
                    ..old
                }
            },
        )
        .map_err(|_| DevTreeError::NotEnoughMemory)
}
