//! Phase 4: deep extent-tree insertion (depth ≥ 2 mutation).
//!
//! Verifies `extent_mut::plan_insert_extent_deep` against:
//! - synthetic 0→1 promotion (inline root → depth-1 with one leaf block),
//! - synthetic 1→2 promotion (depth-1 root + leaf split → depth-2 root),
//! - read-side round-trip via `extent::lookup` walking through the planner's
//!   block_writes,
//! - allocator interaction (every `alloc()` result is recorded),
//! - depth-cap rejection at `EXT4_EXT_MAX_DEPTH`.

use fs_ext4::block_io::BlockDevice;
use fs_ext4::error::{Error, Result};
use fs_ext4::extent::{
    self, Extent, ExtentHeader, EXT4_EXT_MAGIC, EXT4_EXT_MAX_DEPTH, EXT4_EXT_NODE_SIZE,
};
use fs_ext4::extent_mut::{plan_insert_extent_deep, DeepInsertPlan, DeepReader};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

const BLOCK_SIZE: u32 = 4096;
/// Reserve 4 trailing bytes for `et_checksum` on every non-root block.
const NODE_BODY: usize = BLOCK_SIZE as usize - 4;
/// Per-block leaf/index entry capacity at 4 KiB blocks: (4096 - 12 header
/// - 4 tail) / 12 = 340.
const NODE_CAP: u16 = ((NODE_BODY - EXT4_EXT_NODE_SIZE) / EXT4_EXT_NODE_SIZE) as u16;

// ---------------------------------------------------------------------------
// In-memory backing store: a HashMap<block_num, bytes>. The planner emits
// (block_num, full_block_bytes) tuples; we apply each one into the store and
// then satisfy descent reads from the same map.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MemStore {
    blocks: Mutex<HashMap<u64, Vec<u8>>>,
}

impl MemStore {
    fn write(&self, block: u64, bytes: Vec<u8>) {
        self.blocks.lock().unwrap().insert(block, bytes);
    }

    fn apply_plan(&self, plan: &DeepInsertPlan) {
        let mut map = self.blocks.lock().unwrap();
        for (b, bytes) in &plan.block_writes {
            map.insert(*b, bytes.clone());
        }
    }
}

impl DeepReader for MemStore {
    fn read_block(&self, block: u64, out: &mut [u8]) -> Result<()> {
        let map = self.blocks.lock().unwrap();
        let bytes = map
            .get(&block)
            .ok_or(Error::CorruptExtentTree("MemStore: missing block"))?;
        let n = out.len().min(bytes.len());
        out[..n].copy_from_slice(&bytes[..n]);
        Ok(())
    }
}

/// Adapter so the same `MemStore` can drive read-side `extent::lookup`.
struct MemDevice<'a> {
    store: &'a MemStore,
    block_size: u32,
}

impl<'a> BlockDevice for MemDevice<'a> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let block = offset / self.block_size as u64;
        self.store.read_block(block, buf)
    }
    fn size_bytes(&self) -> u64 {
        u64::MAX
    }
}

// ---------------------------------------------------------------------------
// Allocator helpers — hand out a strictly-increasing sequence of physical
// block numbers and return the same numbers as `expected` for comparison.
// ---------------------------------------------------------------------------

fn make_alloc(start: u64) -> impl FnMut() -> Result<u64> {
    let mut next = start;
    move || {
        let b = next;
        next += 1;
        Ok(b)
    }
}

// ---------------------------------------------------------------------------
// Helpers to build inline roots and full blocks by hand for the depth-cap
// test where we need to fabricate a maximum-depth tree without first growing
// it organically (which would take prohibitively many inserts).
// ---------------------------------------------------------------------------

fn ext(log: u32, len: u16, phys: u64) -> Extent {
    Extent {
        logical_block: log,
        length: len,
        physical_block: phys,
        uninitialized: false,
    }
}

fn build_inline_leaf_root(extents: &[Extent]) -> Vec<u8> {
    let mut out = vec![0u8; 60];
    out[0..2].copy_from_slice(&EXT4_EXT_MAGIC.to_le_bytes());
    out[2..4].copy_from_slice(&(extents.len() as u16).to_le_bytes());
    out[4..6].copy_from_slice(&4u16.to_le_bytes());
    out[6..8].copy_from_slice(&0u16.to_le_bytes());
    out[8..12].copy_from_slice(&0u32.to_le_bytes());
    for (i, e) in extents.iter().enumerate() {
        let off = EXT4_EXT_NODE_SIZE * (1 + i);
        out[off..off + 4].copy_from_slice(&e.logical_block.to_le_bytes());
        out[off + 4..off + 6].copy_from_slice(&e.length.to_le_bytes());
        let phys_hi = ((e.physical_block >> 32) & 0xFFFF) as u16;
        let phys_lo = (e.physical_block & 0xFFFF_FFFF) as u32;
        out[off + 6..off + 8].copy_from_slice(&phys_hi.to_le_bytes());
        out[off + 8..off + 12].copy_from_slice(&phys_lo.to_le_bytes());
    }
    out
}

fn build_inline_index_root(depth: u16, indices: &[(u32, u64)]) -> Vec<u8> {
    let mut out = vec![0u8; 60];
    out[0..2].copy_from_slice(&EXT4_EXT_MAGIC.to_le_bytes());
    out[2..4].copy_from_slice(&(indices.len() as u16).to_le_bytes());
    out[4..6].copy_from_slice(&4u16.to_le_bytes());
    out[6..8].copy_from_slice(&depth.to_le_bytes());
    out[8..12].copy_from_slice(&0u32.to_le_bytes());
    for (i, (logical, child)) in indices.iter().enumerate() {
        let off = EXT4_EXT_NODE_SIZE * (1 + i);
        let leaf_lo = (*child & 0xFFFF_FFFF) as u32;
        let leaf_hi = ((*child >> 32) & 0xFFFF) as u16;
        out[off..off + 4].copy_from_slice(&logical.to_le_bytes());
        out[off + 4..off + 8].copy_from_slice(&leaf_lo.to_le_bytes());
        out[off + 8..off + 10].copy_from_slice(&leaf_hi.to_le_bytes());
    }
    out
}

fn build_full_index_block(depth: u16, indices: &[(u32, u64)]) -> Vec<u8> {
    let mut out = vec![0u8; BLOCK_SIZE as usize];
    out[0..2].copy_from_slice(&EXT4_EXT_MAGIC.to_le_bytes());
    out[2..4].copy_from_slice(&(indices.len() as u16).to_le_bytes());
    out[4..6].copy_from_slice(&NODE_CAP.to_le_bytes());
    out[6..8].copy_from_slice(&depth.to_le_bytes());
    out[8..12].copy_from_slice(&0u32.to_le_bytes());
    for (i, (logical, child)) in indices.iter().enumerate() {
        let off = EXT4_EXT_NODE_SIZE * (1 + i);
        let leaf_lo = (*child & 0xFFFF_FFFF) as u32;
        let leaf_hi = ((*child >> 32) & 0xFFFF) as u16;
        out[off..off + 4].copy_from_slice(&logical.to_le_bytes());
        out[off + 4..off + 8].copy_from_slice(&leaf_lo.to_le_bytes());
        out[off + 8..off + 10].copy_from_slice(&leaf_hi.to_le_bytes());
    }
    out
}

fn build_full_leaf_block(extents: &[Extent]) -> Vec<u8> {
    let mut out = vec![0u8; BLOCK_SIZE as usize];
    out[0..2].copy_from_slice(&EXT4_EXT_MAGIC.to_le_bytes());
    out[2..4].copy_from_slice(&(extents.len() as u16).to_le_bytes());
    out[4..6].copy_from_slice(&NODE_CAP.to_le_bytes());
    out[6..8].copy_from_slice(&0u16.to_le_bytes());
    out[8..12].copy_from_slice(&0u32.to_le_bytes());
    for (i, e) in extents.iter().enumerate() {
        let off = EXT4_EXT_NODE_SIZE * (1 + i);
        out[off..off + 4].copy_from_slice(&e.logical_block.to_le_bytes());
        out[off + 4..off + 6].copy_from_slice(&e.length.to_le_bytes());
        let phys_hi = ((e.physical_block >> 32) & 0xFFFF) as u16;
        let phys_lo = (e.physical_block & 0xFFFF_FFFF) as u32;
        out[off + 6..off + 8].copy_from_slice(&phys_hi.to_le_bytes());
        out[off + 8..off + 12].copy_from_slice(&phys_lo.to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Drive repeated inserts through `plan_insert_extent_deep`, applying each
/// plan into the in-memory store. Returns the live root bytes after the
/// final insert.
fn run_inserts(
    initial_root: Vec<u8>,
    inserts: &[Extent],
    store: &MemStore,
    alloc_start: u64,
) -> (Vec<u8>, Vec<u64>) {
    let mut root = initial_root;
    let mut alloc = make_alloc(alloc_start);
    let mut all_allocated: Vec<u64> = Vec::new();
    for (i, e) in inserts.iter().enumerate() {
        let plan = plan_insert_extent_deep(&root, *e, BLOCK_SIZE, store, &mut alloc)
            .unwrap_or_else(|err| panic!("insert #{i} ({e:?}): {err}"));
        store.apply_plan(&plan);
        root = plan.new_root;
        all_allocated.extend_from_slice(&plan.allocated_blocks);
    }
    (root, all_allocated)
}

/// Build an inserts list of N contiguous-but-non-mergeable extents — each
/// extent has a unique physical run that doesn't touch its neighbours, so
/// the planner cannot merge them away. Logical blocks are spaced by `step`
/// and physical blocks by 10 (so consecutive entries are never adjacent).
fn non_mergeable_inserts(n: usize) -> Vec<Extent> {
    (0..n)
        .map(|i| {
            ext(
                (i as u32) * 16, // logical step of 16
                1,               // length 1
                1_000_000 + (i as u64) * 10,
            )
        })
        .collect()
}

#[test]
fn inline_root_promotes_to_depth_1_then_depth_2() {
    let store = MemStore::default();
    let initial = build_inline_leaf_root(&[]);

    // Phase A: 4 inserts fill the inline root (depth=0, entries=4).
    let phase_a = non_mergeable_inserts(4);
    let (root_after_a, _) = run_inserts(initial, &phase_a, &store, 100_000);
    let hdr = ExtentHeader::parse(&root_after_a).unwrap();
    assert_eq!(hdr.depth, 0);
    assert_eq!(hdr.entries, 4);

    // Phase B: 5th insert triggers 0→1 promotion.
    let promo = vec![ext(4 * 16, 1, 1_000_040)];
    let (root_after_b, allocated_b) = run_inserts(root_after_a, &promo, &store, 200_000);
    let hdr = ExtentHeader::parse(&root_after_b).unwrap();
    assert_eq!(hdr.depth, 1, "0→1 promotion fires");
    assert_eq!(hdr.entries, 1);
    assert_eq!(allocated_b.len(), 1, "exactly one leaf block allocated");

    // Phase C: keep inserting (monotonic logical blocks → always sorts to
    // the right) until the depth-1 root overflows and 1→2 promotion fires.
    // Each leaf split adds one index entry to the inline root; with cap=4,
    // we need 4 splits to grow root entries to 5 → forces root promotion.
    // Generous cap of 4*NODE_CAP inserts is plenty.
    let mut root = root_after_b;
    let mut total_allocated = 0usize;
    let mut alloc = make_alloc(300_000u64);
    let logical_base = 5u32 * 16;
    let phys_base = 1_000_050u64;
    let max_inserts = 4 * NODE_CAP as usize;
    let mut promoted_to_depth_2 = false;
    for i in 0..max_inserts {
        let e = ext(
            logical_base + (i as u32) * 16,
            1,
            phys_base + (i as u64) * 10,
        );
        let plan =
            plan_insert_extent_deep(&root, e, BLOCK_SIZE, &store, &mut alloc).expect("insert");
        store.apply_plan(&plan);
        total_allocated += plan.allocated_blocks.len();
        root = plan.new_root;
        let hdr = ExtentHeader::parse(&root).unwrap();
        if hdr.depth == 2 {
            promoted_to_depth_2 = true;
            break;
        }
    }
    assert!(promoted_to_depth_2, "1→2 promotion never fired");
    let hdr = ExtentHeader::parse(&root).unwrap();
    assert_eq!(hdr.depth, 2);
    assert_eq!(
        hdr.entries, 2,
        "root holds two index entries post-promotion"
    );
    // Sanity: the promotion phase allocated >= 3 blocks (1 leaf split that
    // overflowed root + 2 fresh root-promotion index blocks). Earlier leaf
    // splits each allocated 1 block.
    assert!(
        total_allocated >= 3,
        "expected >=3 allocations across phase C, got {total_allocated}"
    );
}

#[test]
fn round_trip_reads_recover_every_inserted_extent() {
    let store = MemStore::default();
    let initial = build_inline_leaf_root(&[]);

    // Insert until depth=2 then a few more for good measure. We need enough
    // for 4 leaf splits + root promotion, so 4*NODE_CAP is plenty.
    let total = 4 * NODE_CAP as usize;
    let inserts = non_mergeable_inserts(total);

    // Apply them through the planner.
    let mut root = initial;
    let mut alloc = make_alloc(500_000u64);
    let mut applied = 0usize;
    let mut reached_depth_2 = false;
    for (i, e) in inserts.iter().enumerate() {
        let plan =
            plan_insert_extent_deep(&root, *e, BLOCK_SIZE, &store, &mut alloc).expect("insert");
        store.apply_plan(&plan);
        root = plan.new_root;
        applied = i + 1;
        let hdr = ExtentHeader::parse(&root).unwrap();
        if hdr.depth == 2 {
            reached_depth_2 = true;
            // Keep going for ~50 more inserts post-promotion to exercise
            // depth-2 leaf-of-leaf rewrites.
            if applied >= total {
                break;
            }
        }
    }
    assert!(
        reached_depth_2,
        "depth-2 never reached after {applied} inserts"
    );
    let hdr = ExtentHeader::parse(&root).unwrap();
    assert!(hdr.depth >= 2, "tree should be at depth >= 2");

    // Walk every inserted extent through the read-side `lookup` and confirm
    // it resolves to the right physical block.
    let dev = MemDevice {
        store: &store,
        block_size: BLOCK_SIZE,
    };
    for (i, want) in inserts.iter().enumerate().take(applied) {
        let got = extent::lookup(&root, &dev, BLOCK_SIZE, want.logical_block as u64)
            .unwrap_or_else(|e| panic!("lookup #{i}: {e}"))
            .unwrap_or_else(|| panic!("lookup #{i}: no mapping"));
        assert_eq!(
            got.physical_block, want.physical_block,
            "insert #{i} {want:?} resolved to wrong physical {got:?}"
        );
        assert_eq!(got.length, want.length, "length mismatch on insert #{i}");
    }

    // collect_all should also enumerate every applied entry.
    let all = extent::collect_all(&root, &dev, BLOCK_SIZE).unwrap();
    assert_eq!(all.len(), applied, "collect_all sees every leaf entry");
}

#[test]
fn allocator_interaction_records_every_block() {
    let store = MemStore::default();
    let initial = build_inline_leaf_root(&[]);

    // Track every block the allocator hands out, parallel to the planner's
    // own `allocated_blocks` accounting.
    let handed_out = RefCell::new(Vec::<u64>::new());
    let mut next = 700_000u64;
    let mut alloc = || -> Result<u64> {
        let b = next;
        next += 1;
        handed_out.borrow_mut().push(b);
        Ok(b)
    };

    // Insert enough to force 0→1 promotion (5th insert) and a few leaf
    // rewrites on top — each should pull blocks from `alloc` only when
    // promotion or split fires.
    let inserts = non_mergeable_inserts(20);
    let mut root = initial;
    let mut planner_recorded: Vec<u64> = Vec::new();
    for (i, e) in inserts.iter().enumerate() {
        let plan = plan_insert_extent_deep(&root, *e, BLOCK_SIZE, &store, &mut alloc)
            .unwrap_or_else(|err| panic!("insert #{i}: {err}"));
        store.apply_plan(&plan);
        planner_recorded.extend_from_slice(&plan.allocated_blocks);
        root = plan.new_root;
    }

    assert_eq!(
        planner_recorded,
        *handed_out.borrow(),
        "every alloc() call must appear in DeepInsertPlan.allocated_blocks in order"
    );
    // For 20 non-mergeable inserts: 4 fit on the inline root, the 5th
    // triggers 0→1 (1 block), the remaining 15 sort onto the leaf without
    // overflow → 1 total alloc.
    assert_eq!(planner_recorded.len(), 1);
}

#[test]
fn depth_cap_rejects_promotion_past_max_depth() {
    // Hand-build a tree already at EXT4_EXT_MAX_DEPTH (5). Each level on
    // the leftmost descent path is saturated (entries == capacity), so any
    // insert that splits the leaf will propagate splits all the way up
    // through 4 saturated internal levels, finally hitting the inline root
    // (also full at 4 entries) — and root promotion would push depth to 6.
    // The planner must reject with a depth-cap CorruptExtentTree error.
    //
    // Trick to keep the tree small: the descent only ever reads the
    // *leftmost* index entry at each level (target_logical = 1 < every
    // padding entry's ei_block, so only index[0] is selected). Padding
    // entries can be header-only — point at unique fake block numbers we
    // never plant in the store; the planner won't try to read them.

    let store = MemStore::default();
    // Padding-entry logical block — must be > target_logical (=1) so descent
    // always picks index[0]. Using a large constant keeps inserts trivial.
    const PAD_LOGICAL: u32 = u32::MAX / 2;
    // Fake "physical" base for padding child pointers (never read).
    let mut fake_block_counter: u64 = 50_000_000;
    let mut fresh_fake = || -> u64 {
        let b = fake_block_counter;
        fake_block_counter += 1;
        b
    };

    // Leaf level (depth 0) — full block, NODE_CAP non-mergeable entries.
    // Logical blocks must all be >= target_logical=1 so the new insert
    // would sort cleanly here without overlap. Use logical 100..100+NODE_CAP.
    let leaf_extents: Vec<Extent> = (0..NODE_CAP as usize)
        .map(|i| ext(100 + (i as u32) * 4, 1, 800_000 + (i as u64) * 10))
        .collect();
    let leaf_block_phys: u64 = 900_000;
    store.write(leaf_block_phys, build_full_leaf_block(&leaf_extents));

    // Depth 1: NODE_CAP index entries. index[0] points at the real leaf;
    // entries[1..] are padding (PAD_LOGICAL → fake child).
    let mut d1_indices: Vec<(u32, u64)> = vec![(100, leaf_block_phys)];
    for _ in 1..NODE_CAP {
        d1_indices.push((PAD_LOGICAL, fresh_fake()));
    }
    let d1_block_phys: u64 = 910_000;
    store.write(d1_block_phys, build_full_index_block(1, &d1_indices));

    // Depths 2, 3, 4: same pattern — leftmost child is the level below,
    // rest are padding.
    let d2_block_phys: u64 = 920_000;
    let mut d2_indices: Vec<(u32, u64)> = vec![(100, d1_block_phys)];
    for _ in 1..NODE_CAP {
        d2_indices.push((PAD_LOGICAL, fresh_fake()));
    }
    store.write(d2_block_phys, build_full_index_block(2, &d2_indices));

    let d3_block_phys: u64 = 930_000;
    let mut d3_indices: Vec<(u32, u64)> = vec![(100, d2_block_phys)];
    for _ in 1..NODE_CAP {
        d3_indices.push((PAD_LOGICAL, fresh_fake()));
    }
    store.write(d3_block_phys, build_full_index_block(3, &d3_indices));

    let d4_block_phys: u64 = 940_000;
    let mut d4_indices: Vec<(u32, u64)> = vec![(100, d3_block_phys)];
    for _ in 1..NODE_CAP {
        d4_indices.push((PAD_LOGICAL, fresh_fake()));
    }
    store.write(d4_block_phys, build_full_index_block(4, &d4_indices));

    // Inline root at depth 5, full (4 entries): index[0] points at the
    // depth-4 block; the other 3 are padding pointing at fake blocks.
    let root_indices = vec![
        (100, d4_block_phys),
        (PAD_LOGICAL, fresh_fake()),
        (PAD_LOGICAL.wrapping_add(1), fresh_fake()),
        (PAD_LOGICAL.wrapping_add(2), fresh_fake()),
    ];
    let root = build_inline_index_root(EXT4_EXT_MAX_DEPTH, &root_indices);
    let hdr = ExtentHeader::parse(&root).unwrap();
    assert_eq!(hdr.depth, EXT4_EXT_MAX_DEPTH);
    assert_eq!(hdr.entries, 4);

    // Insert at logical 1 — sorts at the very front of the leftmost leaf,
    // overflows it, propagates splits up through 4 saturated index levels,
    // and finally hits the saturated inline root → would need depth=6.
    let mut alloc = make_alloc(2_000_000);
    let new = ext(1, 1, 9_999_999);
    let err = plan_insert_extent_deep(&root, new, BLOCK_SIZE, &store, &mut alloc)
        .map(|_| ())
        .expect_err("must reject depth-6 promotion");
    match err {
        Error::CorruptExtentTree(msg) => {
            assert!(
                msg.contains("depth") || msg.contains("maximum"),
                "expected depth-cap message, got: {msg}"
            );
        }
        other => panic!("wrong error variant: {other}"),
    }
}
