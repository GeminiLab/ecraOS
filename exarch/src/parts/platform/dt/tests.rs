//! Device-tree queries against small, aligned firmware fixtures.

use alloc::{vec, vec::Vec};

use fdt_rs::{base::DevTree, error::DevTreeError, index::DevTreeIndex, prelude::*};

use super::{
    DeviceTreeNodeExt,
    index::{DeviceTreeIndexExt, DeviceTreeIndexNodeExt, with_index, with_index_in},
};

mod reserved;

/// An aligned DTB with CPU, interrupt-controller, and disabled-device nodes.
struct TestTree {
    /// Aligned storage for the complete device-tree blob.
    words: Vec<u64>,
    /// The DTB length before padding the backing allocation.
    len: usize,
}

impl TestTree {
    /// Creates a small tree with nested nodes and optional malformed compatible data.
    fn new(malformed_compatible: bool) -> Self {
        let mut structure = Vec::new();
        let mut strings = Vec::new();
        begin_node(&mut structure, b"");
        begin_node(&mut structure, b"cpus");
        property(
            &mut structure,
            &mut strings,
            "#address-cells",
            &2u32.to_be_bytes(),
        );

        begin_node(&mut structure, b"cpu@3");
        property(&mut structure, &mut strings, "device_type", b"cpu\0");
        property(
            &mut structure,
            &mut strings,
            "compatible",
            b"arm,cpu\0vendor,cpu\0",
        );
        property(&mut structure, &mut strings, "status", b"okay\0");
        property(
            &mut structure,
            &mut strings,
            "reg",
            &0x1234_5678_0000_0003u64.to_be_bytes(),
        );
        property(&mut structure, &mut strings, "phandle", &7u32.to_be_bytes());
        property(&mut structure, &mut strings, "empty", &[]);
        property(&mut structure, &mut strings, "short", &[1, 2, 3]);
        begin_node(&mut structure, b"interrupt-controller");
        property(&mut structure, &mut strings, "interrupt-controller", &[]);
        word(&mut structure, 2);
        word(&mut structure, 2);

        begin_node(&mut structure, b"cpu@4");
        property(&mut structure, &mut strings, "device_type", b"cpu\0");
        property(&mut structure, &mut strings, "compatible", b"arm,cpu\0");
        property(&mut structure, &mut strings, "status", b"disabled\0");
        word(&mut structure, 2);
        word(&mut structure, 2);

        begin_node(&mut structure, b"soc");
        begin_node(&mut structure, b"interrupt-controller@1000");
        property(
            &mut structure,
            &mut strings,
            "compatible",
            b"vendor,gic\0arm,gic-v3\0",
        );
        word(&mut structure, 2);
        word(&mut structure, 2);

        begin_node(&mut structure, b"malformed-status");
        property(&mut structure, &mut strings, "status", b"okay");
        word(&mut structure, 2);
        if malformed_compatible {
            begin_node(&mut structure, b"malformed-compatible");
            property(&mut structure, &mut strings, "compatible", b"unterminated");
            word(&mut structure, 2);
        }
        word(&mut structure, 2);
        word(&mut structure, 9);

        Self::from_parts(structure, strings, &[])
    }

    /// Encodes a tree structure, string block, and memory-reservation entries as an aligned DTB.
    fn from_parts(structure: Vec<u8>, strings: Vec<u8>, reservations: &[(u64, u64)]) -> Self {
        let mut reservation_map = Vec::new();
        for (address, size) in reservations {
            reservation_map.extend_from_slice(&address.to_be_bytes());
            reservation_map.extend_from_slice(&size.to_be_bytes());
        }
        reservation_map.extend_from_slice(&[0; 16]);

        let structure_offset = 40 + reservation_map.len();
        let strings_offset = structure_offset + structure.len();
        let len = strings_offset + strings.len();
        let mut bytes = Vec::new();
        for value in [
            0xd00d_feed,
            len as u32,
            structure_offset as u32,
            strings_offset as u32,
            40,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            word(&mut bytes, value);
        }
        bytes.extend_from_slice(&reservation_map);
        bytes.extend_from_slice(&structure);
        bytes.extend_from_slice(&strings);

        let mut words = vec![0u64; len.div_ceil(core::mem::size_of::<u64>())];
        // SAFETY: words has room for all bytes and is disjoint from the source allocation.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast::<u8>(), len);
        }
        Self { words, len }
    }

    /// Borrows this fixture as a valid, aligned device tree.
    fn tree(&self) -> DevTree<'_> {
        // SAFETY: words owns at least len bytes and is aligned to u64.
        let bytes =
            unsafe { core::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.len) };
        // SAFETY: new constructs a complete FDT with exactly len bytes.
        unsafe { DevTree::new(bytes) }.expect("fixture DTB must parse")
    }
}

/// Appends a big-endian word to a DTB section.
fn word(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_be_bytes());
}

/// Starts a node and pads its name to a 32-bit boundary.
fn begin_node(buffer: &mut Vec<u8>, name: &[u8]) {
    word(buffer, 1);
    buffer.extend_from_slice(name);
    buffer.push(0);
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }
}

/// Appends a named property and records its name in the strings section.
fn property(buffer: &mut Vec<u8>, strings: &mut Vec<u8>, name: &str, value: &[u8]) {
    word(buffer, 3);
    word(buffer, value.len() as u32);
    word(buffer, strings.len() as u32);
    strings.extend_from_slice(name.as_bytes());
    strings.push(0);
    buffer.extend_from_slice(value);
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }
}

/// Checks the same property, status, and name semantics for both node representations.
fn assert_cpu_queries<'dt>(cpu: &impl DeviceTreeNodeExt<'dt>) {
    assert_eq!(cpu.node_name().unwrap(), "cpu@3");
    assert_eq!(cpu.node_kind().unwrap(), "cpu");
    assert!(cpu.is_node_kind("cpu").unwrap());
    assert!(!cpu.is_node_kind("cp").unwrap());
    assert!(cpu.is_device_type("cpu").unwrap());
    assert!(cpu.is_compatible("vendor,cpu").unwrap());
    assert!(!cpu.is_compatible("absent").unwrap());
    assert!(cpu.is_enabled().unwrap());
    assert!(cpu.has_property("empty").unwrap());
    assert!(!cpu.has_property("missing").unwrap());
    assert_eq!(cpu.string_property("device_type").unwrap(), Some("cpu"));
    assert_eq!(cpu.u32_property("reg", 1).unwrap(), Some(3));
    assert_eq!(
        cpu.u64_property("reg", 0).unwrap(),
        Some(0x1234_5678_0000_0003)
    );
    assert_eq!(cpu.phandle_property("phandle").unwrap(), Some(7));
    assert_eq!(cpu.u64_property("missing", 0).unwrap(), None);
    assert_eq!(cpu.phandle_property("missing").unwrap(), None);
    assert!(cpu.u32_property("short", 0).is_err());
    assert!(cpu.u64_property("reg", 1).is_err());
    assert!(cpu.phandle_property("short").is_err());
    for index in [1, usize::MAX / 8, usize::MAX / 8 + 1, usize::MAX] {
        assert_eq!(
            cpu.u64_property("reg", index),
            Err(DevTreeError::InvalidOffset),
        );
    }
    for index in [2, usize::MAX / 4, usize::MAX / 4 + 1, usize::MAX] {
        assert_eq!(
            cpu.u32_property("reg", index),
            Err(DevTreeError::InvalidOffset),
        );
    }
}

/// Preserves the common node contract when switching between streaming and indexed traversal.
#[test]
fn node_queries_agree_for_streaming_and_indexed_nodes() {
    let fixture = TestTree::new(false);
    let tree = fixture.tree();
    let cpu = tree
        .nodes()
        .find(|node| Ok(node.node_name()? == "cpu@3"))
        .unwrap()
        .unwrap();
    assert_cpu_queries(&cpu);

    with_index(tree, |index| {
        let cpu = index.find_path("/cpus/cpu@3")?.unwrap();
        assert_cpu_queries(&cpu);
        Ok(())
    })
    .unwrap();
}

/// Restricts child queries to one level and keeps disabled nodes visible.
#[test]
fn hierarchy_queries_match_only_direct_children() {
    let fixture = TestTree::new(false);
    with_index(fixture.tree(), |index| {
        let root = index.root();
        let cpus = root.child_by_kind("cpus")?.unwrap();
        assert_eq!(cpus.child_by_kind("cpu")?.unwrap().node_name()?, "cpu@3");
        assert!(cpus.child_by_name("cpu")?.is_none());
        assert_eq!(cpus.child_by_name("cpu@4")?.unwrap().node_name()?, "cpu@4");
        assert!(!cpus.child_by_name("cpu@4")?.unwrap().is_enabled()?);
        assert_eq!(
            cpus.child_by_compatible("vendor,cpu")?
                .unwrap()
                .node_name()?,
            "cpu@3"
        );
        assert!(root.child_by_compatible("arm,gic-v3")?.is_none());
        assert!(root.child_by_kind("interrupt-controller")?.is_none());
        assert!(
            cpus.child_by_name("cpu@3")?
                .unwrap()
                .child_by_kind("cpu")?
                .is_none()
        );
        assert!(root.is_enabled()?);
        assert!(!root.is_node_kind("cpus")?);
        Ok(())
    })
    .unwrap();
}

/// Resolves complete path components without accepting relative paths or partial unit addresses.
#[test]
fn paths_match_complete_names_and_validate_syntax() {
    let fixture = TestTree::new(false);
    with_index(fixture.tree(), |index| {
        assert!(index.find_path("/")?.unwrap() == index.root());
        assert_eq!(
            index
                .find_path("/cpus/cpu@3/interrupt-controller")?
                .unwrap()
                .node_name()?,
            "interrupt-controller"
        );
        assert_eq!(
            index.find_compatible("arm,gic-v3")?.unwrap().node_name()?,
            "interrupt-controller@1000"
        );
        for path in ["/cpus/cpu", "/cpus/cpu@30", "/soc/cpu@3", "/absent"] {
            assert!(
                index.find_path(path)?.is_none(),
                "unexpected node for {path}"
            );
        }
        for path in [
            "",
            "cpus",
            "/cpus/",
            "//cpus",
            "/cpus//cpu@3",
            "/cpus/./cpu@3",
            "/cpus/../soc",
        ] {
            assert!(
                matches!(
                    index.find_path(path),
                    Err(DevTreeError::InvalidParameter(_))
                ),
                "invalid path accepted: {path}"
            );
        }
        Ok(())
    })
    .unwrap();
}

/// Keeps malformed properties distinct from absent or disabled properties in both representations.
#[test]
fn queries_preserve_property_errors() {
    let fixture = TestTree::new(true);
    let tree = fixture.tree();
    let node = tree
        .nodes()
        .find(|node| node.is_node_kind("malformed-status"))
        .unwrap()
        .unwrap();
    assert!(node.is_enabled().is_err());
    let node = tree
        .nodes()
        .find(|node| node.is_node_kind("malformed-compatible"))
        .unwrap()
        .unwrap();
    assert!(node.is_compatible("absent").is_err());
    with_index(tree, |index| {
        assert!(
            index
                .find_path("/malformed-status")?
                .unwrap()
                .is_enabled()
                .is_err()
        );
        assert!(index.find_compatible("absent").is_err());
        assert!(index.root().child_by_compatible("absent").is_err());
        Ok(())
    })
    .unwrap();
}

/// Rejects insufficient storage before running a query and allows buffer reuse after an error.
#[test]
fn index_scope_propagates_errors_and_releases_the_buffer() {
    let fixture = TestTree::new(false);
    let tree = fixture.tree();
    let mut ran = false;
    let result = with_index_in(tree, &mut [], |_| {
        ran = true;
        Ok(())
    });
    assert_eq!(result, Err(DevTreeError::NotEnoughMemory));
    assert!(!ran);

    let layout = DevTreeIndex::get_layout(&tree).unwrap();
    let mut buffer = vec![0; layout.size() + layout.align()];
    assert_eq!(
        with_index_in(tree, &mut buffer, |_| Err::<(), _>(
            DevTreeError::ParseError
        )),
        Err(DevTreeError::ParseError)
    );
    let name = with_index_in(tree, &mut buffer, |index| {
        index.find_path("/cpus/cpu@3")?.unwrap().node_name()
    })
    .unwrap();
    assert_eq!(name, "cpu@3");
    assert_eq!(
        with_index(tree, |_| Err::<(), _>(DevTreeError::ParseError)),
        Err(DevTreeError::ParseError)
    );
}
