//! Allocation-free queries over a flattened device tree.

use fdt_rs::{
    base::{DevTreeNode, DevTreeProp},
    error::DevTreeError,
    prelude::{FallibleIterator, PropReader},
};

/// Convenient allocation-free queries for parsed device-tree nodes.
///
/// The trait extends the streaming node representation. Its methods only inspect the node's
/// existing properties and never construct an index or allocate a scratch buffer.
///
/// Implementations preserve parser errors and do not mutate the device tree.
pub trait DeviceTreeNodeExt<'dt> {
    /// The property handle returned by this node representation.
    type Property: PropReader<'dt>;

    /// Finds a property by its exact name.
    ///
    /// Returns no property when the node has no exact name match. Malformed properties or names
    /// are returned as parsing errors instead of being treated as a non-match.
    ///
    /// # Errors
    ///
    /// Returns an error when the property iterator or a property name is malformed.
    fn property(&self, name: &str) -> Result<Option<Self::Property>, DevTreeError>;

    /// Reads a string property when it exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the property value is not a valid device-tree string.
    fn string_property(&self, name: &str) -> Result<Option<&'dt str>, DevTreeError> {
        self.property(name)?
            .map(|property| property.str())
            .transpose()
    }

    /// Reads one 32-bit cell from a property when it exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the property is malformed or does not contain the requested cell.
    fn u32_property(&self, name: &str, index: usize) -> Result<Option<u32>, DevTreeError> {
        self.property(name)?
            .map(|property| property.u32(index))
            .transpose()
    }

    /// Reports whether the node declares the requested device type.
    ///
    /// # Errors
    ///
    /// Returns an error when the device-type property is malformed.
    fn is_device_type(&self, value: &str) -> Result<bool, DevTreeError> {
        Ok(self.string_property("device_type")? == Some(value))
    }

    /// Reports whether the node's compatible string list contains the requested value.
    ///
    /// # Errors
    ///
    /// Returns an error when the compatible property contains malformed strings.
    fn is_compatible(&self, value: &str) -> Result<bool, DevTreeError> {
        let Some(property) = self.property("compatible")? else {
            return Ok(false);
        };
        let mut values = property.iter_str();
        while let Some(candidate) = values.next()? {
            if candidate == value {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Reports whether firmware marks the node as enabled.
    ///
    /// Missing status, "okay", and "ok" mean enabled. Other valid strings mean disabled.
    /// Malformed values are returned as parsing errors.
    ///
    /// # Errors
    ///
    /// Returns an error when the status property is malformed.
    fn is_enabled(&self) -> Result<bool, DevTreeError> {
        Ok(matches!(
            self.string_property("status")?,
            None | Some("okay" | "ok")
        ))
    }
}

impl<'a, 'dt: 'a> DeviceTreeNodeExt<'dt> for DevTreeNode<'a, 'dt> {
    type Property = DevTreeProp<'a, 'dt>;

    /// Finds a property by walking this node's streaming property iterator.
    fn property(&self, name: &str) -> Result<Option<Self::Property>, DevTreeError> {
        let mut properties = self.props();
        while let Some(property) = properties.next()? {
            if property.name()? == name {
                return Ok(Some(property));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use fdt_rs::{base::DevTree, prelude::*};

    use super::DeviceTreeNodeExt;

    /// Adds one big-endian 32-bit word to a byte buffer.
    fn push_word(buffer: &mut Vec<u8>, value: u32) {
        buffer.extend_from_slice(&value.to_be_bytes());
    }

    /// Adds an FDT node name and pads it to a 32-bit boundary.
    fn push_name(buffer: &mut Vec<u8>, name: &[u8]) {
        buffer.extend_from_slice(name);
        buffer.push(0);
        while buffer.len() % 4 != 0 {
            buffer.push(0);
        }
    }

    /// Adds one FDT property with its name-table offset and raw value.
    fn push_property(buffer: &mut Vec<u8>, name_offset: u32, value: &[u8]) {
        push_word(buffer, 3);
        push_word(buffer, value.len() as u32);
        push_word(buffer, name_offset);
        buffer.extend_from_slice(value);
        while buffer.len() % 4 != 0 {
            buffer.push(0);
        }
    }

    /// Builds a compact aligned DTB containing CPU, disabled, and status-less nodes.
    fn test_dtb() -> (Vec<u64>, usize) {
        let mut structure = Vec::new();
        push_word(&mut structure, 1);
        push_name(&mut structure, b"");

        push_word(&mut structure, 1);
        push_name(&mut structure, b"cpu@3");
        push_property(&mut structure, 0, b"cpu\0");
        push_property(&mut structure, 12, b"arm,cpu\0vendor,cpu\0");
        push_property(&mut structure, 23, b"okay\0");
        push_property(&mut structure, 30, &[0, 0, 0, 3]);
        push_word(&mut structure, 2);

        push_word(&mut structure, 1);
        push_name(&mut structure, b"disabled@4");
        push_property(&mut structure, 23, b"disabled\0");
        push_word(&mut structure, 2);

        push_word(&mut structure, 1);
        push_name(&mut structure, b"status-less@5");
        push_word(&mut structure, 2);

        push_word(&mut structure, 1);
        push_name(&mut structure, b"malformed-status@6");
        push_property(&mut structure, 23, b"okay");
        push_word(&mut structure, 2);

        push_word(&mut structure, 2);
        push_word(&mut structure, 9);

        let strings = b"device_type\0compatible\0status\0reg\0";
        let structure_offset = 56;
        let strings_offset = structure_offset + structure.len();
        let total_size = strings_offset + strings.len();
        let mut bytes = Vec::with_capacity(total_size);
        for word in [
            0xd00d_feed,
            total_size as u32,
            structure_offset as u32,
            strings_offset as u32,
            40,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            push_word(&mut bytes, word);
        }
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&structure);
        bytes.extend_from_slice(strings);

        let mut aligned = vec![0u64; total_size.div_ceil(core::mem::size_of::<u64>())];
        // SAFETY: the destination is large enough for total_size bytes and does not overlap bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                aligned.as_mut_ptr().cast::<u8>(),
                total_size,
            );
        }
        (aligned, total_size)
    }

    /// Reads common properties from base nodes without constructing a device-tree index.
    #[test]
    fn reads_properties_and_device_tree_node_semantics() {
        let (aligned, len) = test_dtb();
        // SAFETY: aligned owns at least len bytes and is aligned to u64.
        let bytes = unsafe { core::slice::from_raw_parts(aligned.as_ptr().cast::<u8>(), len) };
        // SAFETY: test_dtb creates a complete FDT in a 64-bit-aligned backing allocation.
        let tree = unsafe { DevTree::new(bytes) }.expect("test DTB must parse");
        let mut nodes = tree.nodes();
        let _root = nodes
            .next()
            .expect("root parse must succeed")
            .expect("root exists");
        let cpu = nodes
            .next()
            .expect("CPU parse must succeed")
            .expect("CPU node exists");
        let disabled = nodes
            .next()
            .expect("disabled-node parse must succeed")
            .expect("disabled node exists");
        let status_less = nodes
            .next()
            .expect("status-less parse must succeed")
            .expect("status-less node exists");
        let malformed_status = nodes
            .next()
            .expect("malformed-status parse must succeed")
            .expect("malformed-status node exists");

        assert_eq!(cpu.string_property("device_type").unwrap(), Some("cpu"));
        assert_eq!(cpu.u32_property("reg", 0).unwrap(), Some(3));
        assert!(cpu.is_device_type("cpu").unwrap());
        assert!(cpu.is_compatible("vendor,cpu").unwrap());
        assert!(cpu.is_enabled().unwrap());
        assert!(!disabled.is_enabled().unwrap());
        assert!(status_less.is_enabled().unwrap());
        assert!(malformed_status.is_enabled().is_err());
        assert_eq!(cpu.string_property("missing").unwrap(), None);
    }
}
