//! Shared queries over a flattened device tree.

use fdt_rs::{
    base::{DevTreeNode, DevTreeProp},
    error::DevTreeError,
    prelude::{FallibleIterator, PropReader},
    spec::Phandle,
};

pub mod index;

#[cfg(test)]
mod tests;

/// Convenient queries for parsed device-tree nodes.
///
/// The trait extends both the streaming and indexed node representations. Its property methods
/// only inspect the node's existing properties. No method allocates memory or builds an index,
/// and returned strings and property handles borrow the existing DTB.
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

    /// Reports whether the node contains a property with the requested name.
    ///
    /// # Errors
    ///
    /// Returns an error when the property iterator or a property name is malformed.
    fn has_property(&self, name: &str) -> Result<bool, DevTreeError> {
        Ok(self.property(name)?.is_some())
    }

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
            .map(|property| {
                // fdt-rs 0.4.5 computes index * 4 and offset + 4 using plain arithmetic.
                // Either can overflow, causing a panic or a wrapped offset to pass its bounds check.
                // Comparing element counts first avoids both overflowing calculations.
                if index >= property.length() / core::mem::size_of::<u32>() {
                    return Err(DevTreeError::InvalidOffset);
                }
                property.u32(index)
            })
            .transpose()
    }

    /// Reads one big-endian 64-bit integer from a property when it exists.
    ///
    /// The index counts 64-bit integers rather than the DTB's 32-bit cells. This does not interpret
    /// address or size cell counts and is not a general decoder for a reg property.
    ///
    /// # Errors
    ///
    /// Returns an error when the property is malformed or does not contain the requested cell.
    fn u64_property(&self, name: &str, index: usize) -> Result<Option<u64>, DevTreeError> {
        self.property(name)?
            .map(|property| {
                // see `u32_property` for the reason checking element counts first
                if index >= property.length() / core::mem::size_of::<u64>() {
                    return Err(DevTreeError::InvalidOffset);
                }
                property.u64(index)
            })
            .transpose()
    }

    /// Reads one phandle from a property when it exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the property is malformed or does not contain the requested phandle.
    fn phandle_property(&self, name: &str) -> Result<Option<Phandle>, DevTreeError> {
        self.property(name)?
            .map(|property| property.phandle(0))
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

    /// Returns the node name including its optional unit address.
    ///
    /// # Errors
    ///
    /// Returns an error when the node name is not valid UTF-8.
    fn node_name(&self) -> Result<&'dt str, DevTreeError>;

    /// Returns the node name before its optional unit address.
    ///
    /// For example, `cpu@0` has the node kind `cpu`.
    ///
    /// # Errors
    ///
    /// Returns an error when the node name is not valid UTF-8.
    fn node_kind(&self) -> Result<&'dt str, DevTreeError> {
        Ok(self.node_name()?.split('@').next().unwrap_or_default())
    }

    /// Reports whether the node has the requested kind.
    ///
    /// # Errors
    ///
    /// Returns an error when the node name is not valid UTF-8.
    fn is_node_kind(&self, kind: &str) -> Result<bool, DevTreeError> {
        Ok(self.node_kind()? == kind)
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

    /// Returns the complete streaming node name.
    fn node_name(&self) -> Result<&'dt str, DevTreeError> {
        self.name()
    }
}
