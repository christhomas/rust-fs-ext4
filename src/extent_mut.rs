//! Extent tree mutation — planning layer (E7).
//!
//! Produces typed [`ExtentMutation`] values describing how the extent tree
//! should change. Does NOT write to disk; E10 composes these into file write
//! operations and E11 journals them.
//!
//! Scope for Phase 4 initial landing:
//! - Operations on the inline root (leaf, depth 0). The inline root carries up
//!   to 4 entries on a standard 60-byte i_block area, which covers the common
//!   case of small files (≤ 4 contiguous extents).
//! - `insert_extent`: place a new leaf extent in sorted position; returns an
//!   error tagged `LEAF_FULL_NEEDS_PROMOTION` when the root is saturated and
//!   the caller must promote to depth > 0 (future task).
//! - `split_extent`: split one extent at a logical-block boundary (used when
//!   partially overwriting, freeing a middle run, or converting uninit→init on
//!   a sub-range).
//! - `merge_adjacent`: fuse two neighbours whose `[log+len, phys+len)` line up.
//! - `free_extent`: drop an entry and report the physical block range to
//!   hand to the block bitmap freer (E5).
//!
//! Multi-level tree mutation (leaf-block split, internal-node rebalance) is
//! deferred. Hitting it returns [`Error::Corrupt`] with a clear message so
//! callers can fall back to copy-on-write or error out cleanly.

use crate::error::{Error, Result};
use crate::extent::{Extent, ExtentHeader, EXT4_EXT_MAGIC, EXT4_EXT_NODE_SIZE, EXT_INIT_MAX_LEN};

/// A primitive change the extent-write path wants to make to the tree. The
/// caller (E10 or a test) converts this into real I/O under a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtentMutation {
    /// Overwrite the inline root (60 bytes of `i_block`) with these bytes.
    WriteRoot { bytes: Vec<u8> },
    /// Allocate a fresh leaf block and write these bytes to it. Returned for
    /// future multi-level operations; not produced by the current plan fns.
    AllocLeafBlock { bytes: Vec<u8> },
    /// Free a previously-used leaf/index block back to the bitmap.
    FreeLeafBlock { block: u64 },
    /// Free a contiguous physical-block run back to the bitmap (from a
    /// dropped leaf extent). `start` is the first physical block, `len` is
    /// the count.
    FreePhysicalRun { start: u64, len: u32 },
}

/// Build a leaf extent entry's 12 bytes.
fn encode_extent(e: &Extent) -> [u8; EXT4_EXT_NODE_SIZE] {
    let mut buf = [0u8; EXT4_EXT_NODE_SIZE];
    buf[0..4].copy_from_slice(&e.logical_block.to_le_bytes());
    let ee_len: u16 = if e.uninitialized {
        e.length + EXT_INIT_MAX_LEN
    } else {
        e.length
    };
    buf[4..6].copy_from_slice(&ee_len.to_le_bytes());
    let phys_hi = ((e.physical_block >> 32) & 0xFFFF) as u16;
    let phys_lo = (e.physical_block & 0xFFFF_FFFF) as u32;
    buf[6..8].copy_from_slice(&phys_hi.to_le_bytes());
    buf[8..12].copy_from_slice(&phys_lo.to_le_bytes());
    buf
}

/// Re-emit the inline root from a header + sorted extent list. `root_len` is
/// the size of the i_block area (always 60 for inline root, larger for a
/// full-block leaf).
fn build_root(header: &ExtentHeader, extents: &[Extent], root_len: usize) -> Vec<u8> {
    let mut out = vec![0u8; root_len];
    // header
    out[0..2].copy_from_slice(&EXT4_EXT_MAGIC.to_le_bytes());
    out[2..4].copy_from_slice(&(extents.len() as u16).to_le_bytes());
    out[4..6].copy_from_slice(&header.max.to_le_bytes());
    out[6..8].copy_from_slice(&header.depth.to_le_bytes());
    out[8..12].copy_from_slice(&header.generation.to_le_bytes());
    for (i, e) in extents.iter().enumerate() {
        let off = EXT4_EXT_NODE_SIZE * (1 + i);
        out[off..off + EXT4_EXT_NODE_SIZE].copy_from_slice(&encode_extent(e));
    }
    out
}

/// Read the leaf entries from an inline root. Errors on non-leaf (the root
/// has descended into index entries — caller must handle the multi-level
/// case separately).
fn read_leaf_entries(root: &[u8]) -> Result<(ExtentHeader, Vec<Extent>)> {
    let header = ExtentHeader::parse(root)?;
    if !header.is_leaf() {
        return Err(Error::CorruptExtentTree(
            "extent_mut: multi-level tree mutation not yet supported",
        ));
    }
    let mut out = Vec::with_capacity(header.entries as usize);
    for i in 0..header.entries {
        let off = EXT4_EXT_NODE_SIZE * (1 + i as usize);
        if off + EXT4_EXT_NODE_SIZE > root.len() {
            return Err(Error::CorruptExtentTree("leaf entry out of range"));
        }
        out.push(Extent::parse(&root[off..off + EXT4_EXT_NODE_SIZE])?);
    }
    Ok((header, out))
}

/// Returns true iff two adjacent leaf extents are physically contiguous and
/// in the same uninit state — candidate for a merge.
fn are_contiguous(a: &Extent, b: &Extent) -> bool {
    if a.uninitialized != b.uninitialized {
        return false;
    }
    let a_log_end = a.logical_block as u64 + a.length as u64;
    let a_phys_end = a.physical_block + a.length as u64;
    a_log_end == b.logical_block as u64 && a_phys_end == b.physical_block
}

/// Plan insertion of `new` into the inline-root leaf. Preserves sort order on
/// `logical_block`, rejects overlaps, and auto-merges with the prev / next
/// entry when contiguous.
///
/// Errors:
/// - `LEAF_FULL_NEEDS_PROMOTION` if there is no room AND no merge was
///   possible. Caller must promote to an internal-root tree (future task).
/// - `extent overlaps existing` if `new`'s logical range overlaps an existing
///   entry.
pub fn plan_insert_extent(root: &[u8], new: Extent) -> Result<Vec<ExtentMutation>> {
    let (header, mut entries) = read_leaf_entries(root)?;

    // Reject overlaps. Assumes entries sorted by logical_block.
    for e in &entries {
        let e_end = e.logical_block as u64 + e.length as u64;
        let n_end = new.logical_block as u64 + new.length as u64;
        if !(n_end <= e.logical_block as u64 || new.logical_block as u64 >= e_end) {
            return Err(Error::CorruptExtentTree("extent overlaps existing"));
        }
    }

    // Find insert position (stable — first index whose logical_block > new).
    let pos = entries
        .iter()
        .position(|e| e.logical_block as u64 > new.logical_block as u64)
        .unwrap_or(entries.len());
    entries.insert(pos, new);

    // Auto-merge with left neighbour if contiguous.
    if pos > 0 && are_contiguous(&entries[pos - 1], &entries[pos]) {
        let right = entries.remove(pos);
        entries[pos - 1].length += right.length;
    }
    // Auto-merge with right neighbour (pos may have shifted — recompute).
    let pos = entries
        .iter()
        .position(|e| {
            e.logical_block == new.logical_block
                || (e.logical_block as u64) < new.logical_block as u64
                    && (e.logical_block as u64 + e.length as u64) > new.logical_block as u64
        })
        .unwrap_or(entries.len().saturating_sub(1));
    if pos + 1 < entries.len() && are_contiguous(&entries[pos], &entries[pos + 1]) {
        let right = entries.remove(pos + 1);
        entries[pos].length += right.length;
    }

    if entries.len() as u16 > header.max {
        return Err(Error::CorruptExtentTree(
            "LEAF_FULL_NEEDS_PROMOTION: root has no slot for new extent",
        ));
    }

    let new_root = build_root(&header, &entries, root.len());
    Ok(vec![ExtentMutation::WriteRoot { bytes: new_root }])
}

/// Plan a split of `entries[idx]` at logical block `split_at` — the entry is
/// replaced by two halves [`start..split_at`, `split_at..end`]. The caller
/// uses this when partially overwriting or converting part of an uninit run.
///
/// Errors if `split_at` is not strictly inside the chosen extent.
pub fn plan_split_extent(root: &[u8], idx: usize, split_at: u32) -> Result<Vec<ExtentMutation>> {
    let (header, mut entries) = read_leaf_entries(root)?;
    let e = *entries
        .get(idx)
        .ok_or(Error::CorruptExtentTree("split idx out of range"))?;
    let start = e.logical_block;
    let end = start + e.length as u32;
    if split_at <= start || split_at >= end {
        return Err(Error::CorruptExtentTree("split point outside extent"));
    }
    let left = Extent {
        logical_block: start,
        length: (split_at - start) as u16,
        physical_block: e.physical_block,
        uninitialized: e.uninitialized,
    };
    let right = Extent {
        logical_block: split_at,
        length: (end - split_at) as u16,
        physical_block: e.physical_block + (split_at - start) as u64,
        uninitialized: e.uninitialized,
    };
    entries[idx] = left;
    entries.insert(idx + 1, right);

    if entries.len() as u16 > header.max {
        return Err(Error::CorruptExtentTree(
            "LEAF_FULL_NEEDS_PROMOTION: split exceeds root capacity",
        ));
    }

    let new_root = build_root(&header, &entries, root.len());
    Ok(vec![ExtentMutation::WriteRoot { bytes: new_root }])
}

/// Plan fusing entries[i] and entries[i+1] when they're physically adjacent
/// and in the same uninit state. No-op (returns empty mutation list) if the
/// pair isn't contiguous.
pub fn plan_merge_adjacent(root: &[u8], i: usize) -> Result<Vec<ExtentMutation>> {
    let (header, mut entries) = read_leaf_entries(root)?;
    if i + 1 >= entries.len() {
        return Err(Error::CorruptExtentTree("merge idx out of range"));
    }
    if !are_contiguous(&entries[i], &entries[i + 1]) {
        return Ok(Vec::new());
    }
    let right = entries.remove(i + 1);
    entries[i].length += right.length;
    let new_root = build_root(&header, &entries, root.len());
    Ok(vec![ExtentMutation::WriteRoot { bytes: new_root }])
}

/// Plan removal of `entries[idx]`: drops the entry from the root and emits a
/// [`ExtentMutation::FreePhysicalRun`] so the caller can free the backing
/// blocks via the bitmap allocator (E5).
pub fn plan_free_extent(root: &[u8], idx: usize) -> Result<Vec<ExtentMutation>> {
    let (header, mut entries) = read_leaf_entries(root)?;
    let removed = entries
        .get(idx)
        .copied()
        .ok_or(Error::CorruptExtentTree("free idx out of range"))?;
    entries.remove(idx);
    let new_root = build_root(&header, &entries, root.len());
    Ok(vec![
        ExtentMutation::WriteRoot { bytes: new_root },
        ExtentMutation::FreePhysicalRun {
            start: removed.physical_block,
            len: removed.length as u32,
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an inline root (60 bytes) for a leaf with the given extents.
    fn mk_root(extents: &[Extent]) -> Vec<u8> {
        let header = ExtentHeader {
            magic: EXT4_EXT_MAGIC,
            entries: extents.len() as u16,
            max: 4,
            depth: 0,
            generation: 0,
        };
        build_root(&header, extents, 60)
    }

    fn ext(log: u32, len: u16, phys: u64, uninit: bool) -> Extent {
        Extent {
            logical_block: log,
            length: len,
            physical_block: phys,
            uninitialized: uninit,
        }
    }

    fn read_back(bytes: &[u8]) -> Vec<Extent> {
        read_leaf_entries(bytes).unwrap().1
    }

    #[test]
    fn insert_into_empty_root() {
        let root = mk_root(&[]);
        let muts = plan_insert_extent(&root, ext(0, 10, 1000, false)).unwrap();
        assert_eq!(muts.len(), 1);
        let ExtentMutation::WriteRoot { bytes } = &muts[0] else {
            panic!("expected WriteRoot");
        };
        let back = read_back(bytes);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].logical_block, 0);
        assert_eq!(back[0].length, 10);
    }

    #[test]
    fn insert_sorted_ordering() {
        let root = mk_root(&[ext(100, 5, 2000, false)]);
        let muts = plan_insert_extent(&root, ext(0, 10, 1000, false)).unwrap();
        let ExtentMutation::WriteRoot { bytes } = &muts[0] else {
            panic!()
        };
        let back = read_back(bytes);
        assert_eq!(back[0].logical_block, 0);
        assert_eq!(back[1].logical_block, 100);
    }

    #[test]
    fn insert_merges_left_contiguous() {
        let root = mk_root(&[ext(0, 10, 1000, false)]);
        // New extent at logical 10, phys 1010 — contiguous with the first.
        let muts = plan_insert_extent(&root, ext(10, 5, 1010, false)).unwrap();
        let ExtentMutation::WriteRoot { bytes } = &muts[0] else {
            panic!()
        };
        let back = read_back(bytes);
        assert_eq!(back.len(), 1, "should merge into single extent");
        assert_eq!(back[0].length, 15);
    }

    #[test]
    fn insert_does_not_merge_across_uninit_boundary() {
        let root = mk_root(&[ext(0, 10, 1000, false)]);
        // Contiguous physically/logically but marked uninitialised — must NOT merge.
        let muts = plan_insert_extent(&root, ext(10, 5, 1010, true)).unwrap();
        let ExtentMutation::WriteRoot { bytes } = &muts[0] else {
            panic!()
        };
        let back = read_back(bytes);
        assert_eq!(back.len(), 2);
        assert!(!back[0].uninitialized);
        assert!(back[1].uninitialized);
    }

    #[test]
    fn insert_rejects_overlap() {
        let root = mk_root(&[ext(0, 10, 1000, false)]);
        let err = plan_insert_extent(&root, ext(5, 5, 2000, false)).unwrap_err();
        match err {
            Error::CorruptExtentTree(msg) => assert!(msg.contains("overlaps")),
            _ => panic!("wrong error kind"),
        }
    }

    #[test]
    fn insert_fails_when_root_full_and_no_merge() {
        let root = mk_root(&[
            ext(0, 5, 1000, false),
            ext(100, 5, 2000, false),
            ext(200, 5, 3000, false),
            ext(300, 5, 4000, false),
        ]);
        let err = plan_insert_extent(&root, ext(500, 5, 5000, false)).unwrap_err();
        match err {
            Error::CorruptExtentTree(msg) => assert!(msg.contains("LEAF_FULL_NEEDS_PROMOTION")),
            _ => panic!("wrong error kind"),
        }
    }

    #[test]
    fn split_extent_at_midpoint() {
        let root = mk_root(&[ext(0, 10, 1000, false)]);
        let muts = plan_split_extent(&root, 0, 4).unwrap();
        let ExtentMutation::WriteRoot { bytes } = &muts[0] else {
            panic!()
        };
        let back = read_back(bytes);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].logical_block, 0);
        assert_eq!(back[0].length, 4);
        assert_eq!(back[0].physical_block, 1000);
        assert_eq!(back[1].logical_block, 4);
        assert_eq!(back[1].length, 6);
        assert_eq!(back[1].physical_block, 1004);
    }

    #[test]
    fn split_rejects_boundary_point() {
        let root = mk_root(&[ext(0, 10, 1000, false)]);
        assert!(plan_split_extent(&root, 0, 0).is_err()); // start
        assert!(plan_split_extent(&root, 0, 10).is_err()); // end
    }

    #[test]
    fn merge_adjacent_contiguous_pair() {
        let root = mk_root(&[
            ext(0, 5, 1000, false),
            ext(5, 5, 1005, false), // contiguous
        ]);
        let muts = plan_merge_adjacent(&root, 0).unwrap();
        let ExtentMutation::WriteRoot { bytes } = &muts[0] else {
            panic!()
        };
        let back = read_back(bytes);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].length, 10);
    }

    #[test]
    fn merge_noop_when_not_contiguous() {
        let root = mk_root(&[
            ext(0, 5, 1000, false),
            ext(5, 5, 5000, false), // physically apart
        ]);
        let muts = plan_merge_adjacent(&root, 0).unwrap();
        assert!(muts.is_empty(), "no-op when not contiguous");
    }

    #[test]
    fn free_extent_emits_physical_run() {
        let root = mk_root(&[ext(0, 5, 1000, false), ext(10, 3, 2000, false)]);
        let muts = plan_free_extent(&root, 0).unwrap();
        assert_eq!(muts.len(), 2);
        match &muts[1] {
            ExtentMutation::FreePhysicalRun { start, len } => {
                assert_eq!(*start, 1000);
                assert_eq!(*len, 5);
            }
            _ => panic!("expected FreePhysicalRun"),
        }
        let ExtentMutation::WriteRoot { bytes } = &muts[0] else {
            panic!()
        };
        let back = read_back(bytes);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].logical_block, 10);
    }

    #[test]
    fn multi_level_tree_refused_cleanly() {
        // Synthetic internal-node root (depth=1).
        let mut buf = vec![0u8; 60];
        buf[0..2].copy_from_slice(&EXT4_EXT_MAGIC.to_le_bytes());
        buf[2..4].copy_from_slice(&0u16.to_le_bytes());
        buf[4..6].copy_from_slice(&4u16.to_le_bytes());
        buf[6..8].copy_from_slice(&1u16.to_le_bytes()); // depth=1 → not leaf
        let err = plan_insert_extent(&buf, ext(0, 1, 1000, false)).unwrap_err();
        match err {
            Error::CorruptExtentTree(msg) => assert!(msg.contains("multi-level")),
            _ => panic!("wrong error kind"),
        }
    }
}
