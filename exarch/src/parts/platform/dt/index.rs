//! Indexed device-tree construction and hierarchy queries.

use alloc::vec;

use fdt_rs::{
    base::DevTree,
    error::DevTreeError,
    index::{DevTreeIndex, DevTreeIndexNode, DevTreeIndexProp},
    prelude::PropReader,
};

use super::DeviceTreeNodeExt;

/// Runs a query while a caller-owned device-tree index buffer is borrowed.
///
/// The index and all nodes obtained from it are valid only for the duration of `query`. This
/// lifetime boundary keeps the index from outliving the buffer that stores it.
///
/// The buffer may be unaligned. A buffer sized to the index layout plus alignment minus one
/// bytes is sufficient regardless of its starting address. This function does not allocate.
///
/// # Errors
///
/// Returns an error when the supplied buffer cannot hold the index or when the query reports a
/// device-tree parsing error.
pub fn with_index_in<'dt, R>(
    dtb: DevTree<'dt>,
    buffer: &mut [u8],
    query: impl for<'i> FnOnce(&DevTreeIndex<'i, 'dt>) -> Result<R, DevTreeError>,
) -> Result<R, DevTreeError> {
    let index = DevTreeIndex::new(dtb, buffer)?;
    query(&index)
}

/// Runs a query with an allocator-backed device-tree index.
///
/// The index buffer is allocated from the kernel allocator and released after `query` returns.
/// Indexed nodes cannot escape the closure because they borrow the temporary index.
///
/// # Errors
///
/// Returns an error when the device tree cannot be indexed or when the query reports a device-tree
/// parsing error. Allocation failure follows the allocator's configured allocation-failure path.
pub fn with_index<'dt, R>(
    dtb: DevTree<'dt>,
    query: impl for<'i> FnOnce(&DevTreeIndex<'i, 'dt>) -> Result<R, DevTreeError>,
) -> Result<R, DevTreeError> {
    let layout = DevTreeIndex::get_layout(&dtb)?;
    let size = layout
        .size()
        .checked_add(layout.align() - 1)
        .ok_or(DevTreeError::NotEnoughMemory)?;
    let mut buffer = vec![0; size];
    with_index_in(dtb, &mut buffer, query)
}

/// Queries an indexed device tree by path or compatible string.
pub trait DeviceTreeIndexExt<'i, 'dt: 'i> {
    /// Finds a node by an absolute device-tree path.
    ///
    /// Path components are matched against complete node names, including unit addresses. The
    /// root path `/` returns the root node. An absent node returns `None`. Empty components and
    /// the special components `.` and `..` are rejected rather than normalized.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is not absolute, contains invalid components, or encounters
    /// a malformed node name.
    fn find_path<'a>(
        &'a self,
        path: &str,
    ) -> Result<Option<DevTreeIndexNode<'a, 'i, 'dt>>, DevTreeError>;

    /// Finds the first node with a compatible string.
    ///
    /// # Errors
    ///
    /// Returns an error when a property name or compatible property is malformed.
    fn find_compatible<'a>(
        &'a self,
        compatible: &str,
    ) -> Result<Option<DevTreeIndexNode<'a, 'i, 'dt>>, DevTreeError>;
}

/// Queries direct children without filtering their enabled status.
pub trait DeviceTreeIndexNodeExt: Sized {
    /// Finds the first direct child with an exact node name.
    ///
    /// The name includes its optional unit address, so `cpu` and `cpu@0` are distinct queries.
    ///
    /// # Errors
    ///
    /// Returns an error when a child node name is malformed.
    fn child_by_name(&self, name: &str) -> Result<Option<Self>, DevTreeError>;

    /// Finds the first direct child with the requested node kind.
    ///
    /// The unit address is ignored, so `child_by_kind("cpu")` matches `cpu@0`.
    ///
    /// # Errors
    ///
    /// Returns an error when a child node name is malformed.
    fn child_by_kind(&self, kind: &str) -> Result<Option<Self>, DevTreeError>;

    /// Finds the first direct child declaring the requested compatible string.
    ///
    /// # Errors
    ///
    /// Returns an error when a child property name or compatible property is malformed.
    fn child_by_compatible(&self, compatible: &str) -> Result<Option<Self>, DevTreeError>;
}

impl<'a, 'i: 'a, 'dt: 'i> DeviceTreeNodeExt<'dt> for DevTreeIndexNode<'a, 'i, 'dt> {
    type Property = DevTreeIndexProp<'a, 'i, 'dt>;

    /// Finds a property by walking the indexed node's property array.
    fn property(&self, name: &str) -> Result<Option<Self::Property>, DevTreeError> {
        for property in self.props() {
            if property.name()? == name {
                return Ok(Some(property));
            }
        }
        Ok(None)
    }

    /// Returns the complete indexed node name.
    fn node_name(&self) -> Result<&'dt str, DevTreeError> {
        self.name()
    }
}

impl<'a, 'i: 'a, 'dt: 'i> DeviceTreeIndexNodeExt for DevTreeIndexNode<'a, 'i, 'dt> {
    /// Finds the first direct child with an exact node name.
    fn child_by_name(&self, name: &str) -> Result<Option<Self>, DevTreeError> {
        for child in self.children() {
            if child.node_name()? == name {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// Finds the first direct child with the requested node kind.
    fn child_by_kind(&self, kind: &str) -> Result<Option<Self>, DevTreeError> {
        for child in self.children() {
            if child.is_node_kind(kind)? {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// Finds the first direct child declaring the requested compatible string.
    fn child_by_compatible(&self, compatible: &str) -> Result<Option<Self>, DevTreeError> {
        for child in self.children() {
            if child.is_compatible(compatible)? {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }
}

impl<'i, 'dt: 'i> DeviceTreeIndexExt<'i, 'dt> for DevTreeIndex<'i, 'dt> {
    /// Finds a node by walking complete path components from the root.
    fn find_path<'a>(
        &'a self,
        path: &str,
    ) -> Result<Option<DevTreeIndexNode<'a, 'i, 'dt>>, DevTreeError> {
        if !path.starts_with('/') {
            return Err(DevTreeError::InvalidParameter(
                "device-tree paths must be absolute",
            ));
        }
        if path == "/" {
            return Ok(Some(self.root()));
        }
        if path
            .split('/')
            .skip(1)
            .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(DevTreeError::InvalidParameter(
                "device-tree paths require nonempty node names without dot components",
            ));
        }

        let mut node = self.root();
        for component in path.split('/').skip(1) {
            let Some(child) = node.child_by_name(component)? else {
                return Ok(None);
            };
            node = child;
        }
        Ok(Some(node))
    }

    /// Finds the first node declaring the requested compatible string.
    fn find_compatible<'a>(
        &'a self,
        compatible: &str,
    ) -> Result<Option<DevTreeIndexNode<'a, 'i, 'dt>>, DevTreeError> {
        for node in self.nodes() {
            if node.is_compatible(compatible)? {
                return Ok(Some(node));
            }
        }
        Ok(None)
    }
}
