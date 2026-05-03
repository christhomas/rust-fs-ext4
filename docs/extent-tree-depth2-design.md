# Phase 4 Design — Depth ≥ 2 Extent Tree Mutation

Status: 2026-05-03. Designed but not yet implemented. The read side
(`extent::lookup`) already traverses arbitrary-depth trees correctly;
this doc covers what the write side (`extent_mut::*`) needs to grow.

## Why this is deferred

The current `extent_mut` operates only on the inline 60-byte root
(depth 0) plus the depth-0→1 promotion case. Files with > 4 extents on
the inline root, or with > ~340 extents on a single leaf block, hit
`Error::CorruptExtentTree("multi-level tree mutation not yet
supported")`. In practice this caps the driver at ~340 contiguous-ish
extents per file.

Implementing depth ≥ 2 properly is a B-tree-style split/merge/promote
algorithm with crash-safety considerations layered on top. It's
straightforward but non-trivial; it warrants a dedicated session rather
than a wedge into a multi-feature stage.

## On-disk layout recap

Every extent block (inline root or full block) has the same 12-byte
header:

```text
0x00 u16 eh_magic       = 0xF30A
0x02 u16 eh_entries     = number of valid entries below
0x04 u16 eh_max         = capacity (depends on container size)
0x06 u16 eh_depth       = 0 for leaf, > 0 for internal index
0x08 u32 eh_generation  = freshness counter (read-only for our purposes)
0x0C ... entries follow, 12 bytes each
```

Leaf entries (`Extent` / `ee_*`):

```text
0x00 u32 ee_block       = first logical block
0x04 u16 ee_len         = length (high bit = uninitialized)
0x06 u16 ee_start_hi
0x08 u32 ee_start_lo
```

Index entries (`ExtentIdx` / `ei_*`):

```text
0x00 u32 ei_block       = first logical block in subtree
0x04 u32 ei_leaf_lo     = physical block of child node
0x08 u16 ei_leaf_hi
0x0A u16 ei_unused      = reserved (zero)
```

Header + entry size are identical (12 bytes); the discriminant is
`eh_depth`.

A non-root block additionally carries a 4-byte
`struct ext4_extent_tail { __le32 et_checksum; }` at the end, on
metadata_csum volumes. `Checksummer::patch_extent_tail` already
handles it; the depth-≥2 writer just has to call it on every freshly-
built non-root block.

## API surface

Add a new module function in `src/extent_mut.rs`:

```rust
/// Insert `new` into the extent tree rooted at `root_bytes` (60 bytes
/// of the inode's i_block). Walks the tree to the correct leaf,
/// splits / promotes as needed, and returns the full set of mutations.
pub fn plan_insert_extent_deep(
    root_bytes: &[u8],
    new: Extent,
    block_size: u32,
    reader: &dyn DeepReader,
    alloc: &mut dyn FnMut() -> Result<u64>,
) -> Result<DeepInsertPlan>;

pub trait DeepReader {
    fn read_block(&self, block: u64, out: &mut [u8]) -> Result<()>;
}

pub struct DeepInsertPlan {
    /// New root bytes (60 bytes) — depth may have changed.
    pub new_root: Vec<u8>,
    /// Block writes: (block_num, full block_size bytes). Includes
    /// existing-but-modified blocks AND newly-allocated blocks.
    pub block_writes: Vec<(u64, Vec<u8>)>,
    /// Newly allocated block numbers — caller marks each in the
    /// bitmap + bumps BGD/SB counters in the same transaction.
    pub allocated_blocks: Vec<u64>,
}
```

The caller (`apply_replace_file_content`, `apply_fallocate_keep_size`,
etc.) drives this through the BlockBuffer + journal writer pattern
already in place.

## Algorithm sketch

```text
fn plan_insert_extent_deep(root, new, ...) -> DeepInsertPlan {
    // 1. Descend from root to the leaf that should hold `new`.
    //    Cache the path of (block_num, raw_bytes, idx_within_parent)
    //    tuples so we can propagate splits back up.
    let path = descend_to_leaf(root, new.logical_block);

    // 2. Try to insert `new` into the leaf.
    //    If it fits + sorts cleanly: build modified leaf bytes,
    //    no propagation needed.
    //    If it overflows: split the leaf in half, allocate a new
    //    leaf block, choose a midpoint logical_block as the new
    //    index entry, and propagate that index entry up to the
    //    parent.

    // 3. Propagate splits up the path. Each level either:
    //    - Has room → insert the new index entry, done.
    //    - Overflows → split the index node, allocate new index
    //      block, propagate one level higher.

    // 4. If the propagation reaches the root (60 bytes inline) and
    //    the root overflows, promote: allocate two index blocks,
    //    move all root entries into them split 50/50, rewrite the
    //    root with depth+1 and 2 index entries.

    // 5. For every node we modified or freshly allocated, patch
    //    `et_checksum` (when metadata_csum is on) and emit a
    //    block_write tuple.
}
```

## Edge cases

- **Maximum depth**: ext4 caps at depth 5 (`EXT4_EXT_MAX_DEPTH`).
  Beyond that the kernel refuses; we should match.
- **Sibling rebalance vs split**: kernel sometimes rebalances between
  full leaves to avoid splits. v1 can skip this — always split, accept
  the slight space inefficiency. Worth measuring before optimizing.
- **Underflow on free**: when an extent is freed and its leaf becomes
  empty, the kernel collapses the leaf and removes the parent's index
  entry; if the parent has only one child, collapse upward too. v1 can
  defer collapse and leave empty leaves — wasteful but correct.
- **Allocator interaction**: every allocated leaf/index block must be
  marked in the buffered bitmap + counted against BGD/SB *in the same
  transaction* as the inode write. The `BlockBuffer` pattern already
  supports this via `buffer_mark_block_run_used` +
  `buffer_patch_bgd_counters`.
- **Extent_tail checksum**: every non-root block's last 4 bytes hold
  `et_checksum`. Build the leaf/index bytes, then call
  `Checksummer::patch_extent_tail(ino, generation, &mut bytes)`
  before staging into the BlockBuffer.

## Testing

- Synthetic: build a file with N+1 contiguous extents where N is the
  inline-root capacity. The (N+1)th insert must trigger 0→1 promotion
  (already works); the 2*(leaf-cap)+1th insert must trigger 1→2
  promotion (the new code path).
- Real: write a 100 MiB heavily-fragmented file (interleave many small
  writes), assert each chunk reads back correctly across remounts.
- Crash sweep: same parameterized budget pattern as
  `tests/journal_writer_truncate_shrink.rs` — each interruption point
  should yield either pre-insert or post-insert state.

## Effort estimate

- Core algorithm: ~400 lines in `extent_mut.rs` (descend + split +
  promote + 4 helpers).
- Test coverage: ~250 lines (synthetic builder + integration + crash
  sweep).
- One full session.

Track here, in `docs/ext4-full-write-support.md` Phase 4, when picked up.
