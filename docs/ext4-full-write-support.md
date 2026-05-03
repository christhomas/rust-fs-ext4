# ext4 Full Write-Support Plan

Status: 2026-05-03. Scope: take rust-fs-ext4 from "scratch-image safe writes"
to **production-grade journaled writes covering the entire ext4 feature
matrix**. Companion to `IMPROVEMENT-PLAN.md` (which focuses on stability
hardening); this doc tracks the *write-feature* surface.

## Current state (read vs write)

| Area | Read | Write |
|---|---|---|
| Extents depth 0 | ✅ | ✅ |
| Extents depth 1 | ✅ | ✅ (promote only) |
| Extents depth ≥ 2 | ✅ | ❌ `extent_mut.rs:87` |
| Inline data | ✅ | ✅ |
| HTree dir traversal | ✅ | ✅ |
| HTree leaf split | n/a | ✅ |
| HTree internal split | n/a | ❌ |
| Linear (non-htree) dirs | ✅ | ✅ |
| Indirect-block dirs (legacy) | ❌ | ❌ |
| Symlinks | ✅ | ✅ |
| Hardlinks | ✅ | ✅ |
| Xattr in-inode | ✅ | ✅ |
| Xattr external block | ✅ | ❌ overflow → ENOSPC |
| ACL | ✅ | via xattr write |
| Sparse reads (holes) | ✅ | n/a |
| Sparse grow (truncate-up) | n/a | ❌ no-op size bump |
| Hole punch / FALLOC_FL_PUNCH_HOLE | n/a | ❌ |
| Truncate shrink | n/a | ✅ |
| File replace-content | n/a | ✅ |
| Append / partial overwrite | n/a | ⚠ replace-only |
| chmod / chown / utimens | n/a | ✅ |
| BGD counter writeback | ✅ | ⚠ inconsistent |
| Superblock counter writeback | ✅ | ⚠ inconsistent |
| Bitmap writeback | ✅ | ✅ |
| Metadata csum verify | ✅ | n/a |
| Metadata csum regenerate (extent tail, dir tail, BGD, inode) | n/a | ⚠ partial |
| JBD2 replay | ✅ | n/a |
| JBD2 live writes | n/a | ❌ |
| Orphan list recovery | ❌ | ❌ |
| fscrypt | ❌ | ❌ |
| verity | ❌ | ❌ |
| quota | ❌ | ❌ |
| casefold | hash only | ❌ |
| Online resize | ❌ | ❌ |

Legend: ✅ = complete, ⚠ = partial, ❌ = not implemented, n/a = does not apply.

---

## Phase 1 — Self-Consistent Allocator (no journal, no new features)

Goal: every successful write leaves bitmap, BGD counters, and SB counters
in agreement. Today some paths drift.

- [x] **1.1 Counter consistency on `free_block_run`** — added
  `Filesystem::free_block_run_and_bgd` helper that frees a run AND
  patches the containing group's BGD per call. All four call sites
  (`apply_truncate_shrink`, `apply_unlink`, `apply_replace_file_content`,
  `apply_rmdir`) routed through it, eliminating the prior single-group
  assumption. SB updated once per high-level op. Pinned by
  `tests/alloc_counter_consistency.rs`.
- [ ] **1.2 Allocator commit helper** — extract a `commit_block_alloc(plan)` /
  `commit_block_free(plan)` pair next to `plan_block_allocation` so every
  caller goes through the same bitmap+BGD+SB sequence. Same for inode
  alloc/free. (Partially done for the free side via 1.1; alloc side still
  has the manual three-call dance — fold into a helper.)
- [ ] **1.3 Audit every `dev.write_at` outside the helpers** — grep for
  raw writes in `fs.rs`, ensure each one is paired with the appropriate
  csum patch (`patch_inode_checksum`, `patch_extent_tail`,
  `patch_dir_block_tail`, `patch_bgd_checksum`).

Acceptance: new test `tests/alloc_counter_consistency.rs` round-trips
1,000 alloc/free cycles and asserts SB+BGD+bitmap agree at every step.

---

## Phase 2 — Sparse Growth & Hole Punching

- [x] **2.1 truncate-up sparse semantics** — already correct via the
  "bump `i_size`, allocate nothing, read returns zeros for unmapped
  logical blocks" path. Pinned by `tests/sparse_grow.rs` (preserves
  head bytes, reads zeros from holes, leaves `i_blocks` unchanged,
  survives remount). `plan_truncate_grow` deliberately stays a size-
  only delta — IMPROVEMENT-PLAN.md item B3 was an outdated worry.
- [x] **2.2 fallocate (FALLOC_FL_KEEP_SIZE)** — `apply_fallocate_keep_size`
  in `src/fs.rs`: allocates a contiguous physical run via
  `plan_block_allocation`, inserts as one uninitialized extent through
  `extent_mut::plan_insert_extent`, commits the bitmap + BGD + SB +
  inode update via `BlockBuffer` (atomic journaled tx). FFI:
  `fs_ext4_fallocate(fs, path, offset, len, flags)` in capi.rs;
  `FS_EXT4_FALLOC_FL_KEEP_SIZE = 0x01`. Pinned by
  `tests/fallocate_keep_size.rs` (4 tests). v1 limits documented:
  no partial overlaps, single contiguous alloc, depth-0 root only.
- [ ] **2.3 fallocate punch-hole (FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE)** —
  FFI returns ENOSYS (78). Needs cross-extent splitting.
- [ ] **2.4 fallocate zero-range (FALLOC_FL_ZERO_RANGE)** — FFI returns
  ENOSYS. Composes from punch-hole + KEEP_SIZE.

Acceptance: sparse 1 MiB file from 4 KiB original reads as
`[orig 4KiB][zeros up to 1MiB]`; `du` reports unchanged block count.

---

## Phase 3 — External Xattr Blocks

- [x] **3.1 Read-side already complete** — `xattr.rs`, `ea_inode.rs`.
- [x] **3.2 Xattr-block checksum recipe** —
  `Checksummer::verify_xattr_block` + `patch_xattr_block` in
  `src/checksum.rs`. Recipe per kernel:
  `crc32c(seed, block_nr_le_u64 || hdr[0..0x10] || [0u32] || hdr[0x14..end])`.
- [x] **3.3 `apply_setxattr` overflow path** — `plan_set_in_external_block`
  in `src/xattr.rs` builds the 32-byte-headered block; `apply_setxattr`
  in `src/fs.rs` falls through from in-inode to external when ENOSPC.
  Allocates fresh block on first overflow; rewrites in place when one
  already exists. Bumps `i_blocks` by one fs-block of sectors. Pinned
  by `tests/xattr_external_block.rs`.
- [x] **3.4 `apply_removexattr` external-block path** —
  `plan_remove_from_external_block` returns `RemovedNowEmpty` when the
  block becomes empty; `apply_removexattr` then frees the block, zeros
  `i_file_acl`, decrements `i_blocks`. Pinned by same test file.
- [ ] **3.5 EA refcount sharing** — if multiple inodes ever share a
  block (refcount > 1), only decrement on remove; only allocate fresh
  when modifying. Deferred (no consumer demand; refcount=1 always).

Acceptance: `tests/xattr_external_block.rs` writes 8 KiB of xattrs to
an inode, reads them back, removes them, asserts block is freed.

---

## Phase 4 — Extent Tree Depth ≥ 2

- [ ] **4.1 Generalize `read_leaf_entries` → `read_node_entries`** —
  `src/extent_mut.rs:83`. Return a `NodeContents::Leaf(Vec<Extent>)` or
  `NodeContents::Index(Vec<ExtentIdx>)` enum.
- [ ] **4.2 `plan_split_index_block`** — allocate a new index block,
  split entries 50/50, update parent's index entries.
- [ ] **4.3 `plan_promote_index`** — when the inline root's index
  children overflow, allocate two index blocks, redistribute, root
  becomes depth+1.
- [ ] **4.4 Recursive descent in `plan_insert_extent`** — climb the
  tree to the correct leaf, perform split, bubble new index entries
  upward, promote root if needed.
- [ ] **4.5 `plan_merge_adjacent` across leaves** — when truncate or
  punch-hole leaves an empty leaf, free it and remove the parent's
  index entry; collapse depth if root becomes single-entry.
- [ ] **4.6 Index-block checksum** — `et_checksum` in the tail of every
  non-root extent block. Reuse `patch_extent_tail`; verify it works for
  index nodes too.

Acceptance: synthetic test builds a file with 1,000 fragmented extents
(spanning 3 levels), reads every byte, truncates, asserts allocator
reclaims everything.

---

## Phase 5 — JBD2 Live-Write Path (the big one)

This is the gate before this driver is safe on real disks.

### 5.1 Journal write infrastructure

- [x] **5.1.1 Journal inode block mapper** — `JournalWriter::open` reads
  the journal inode's extent tree at mount and caches the full
  `physical_map: Vec<u64>`. Constant-time logical→physical lookup
  thereafter.
- [x] **5.1.2 Journal space allocator (simplified)** — for the
  initial landing the writer uses a single-transaction-at-rest model:
  always writes at journal block 1, immediately checkpoints to clean.
  Ring-style batching is deferred to Phase 8 perf.
- [x] **5.1.3 `JournalWriter::commit(dev, &Transaction)`** —
  four-fence protocol (journal-write → mark dirty → final-write → mark
  clean) with explicit flushes between each step. See module docs in
  `src/journal_writer.rs` for the crash-safety analysis.
- [x] **5.1.4 Crash test harness** — `CrashDevice` wrapper in
  `tests/journal_writer_crash_safety.rs` drops writes after a configured
  budget. Parameterized sweep over budgets 0..=20 proves that for every
  interruption point during a chmod, the post-remount state is either
  pre-op or post-op — never torn. Foundation for fault-injection
  coverage of the multi-block ops.

### 5.2 Per-op transaction wrappers

Each existing mutating fn gains a transaction:

- [x] **5.2.1 `apply_chmod`** — wired through `Filesystem::journal`
  (`Option<Mutex<JournalWriter>>` set up at mount). Routes through
  `commit_inode_write` helper which builds the inode-table block,
  splices the new inode bytes, and commits via the writer. Pinned by
  `tests/journal_writer_chmod.rs`.
- [x] **5.2.2 `apply_chown`** — same `commit_inode_write` route.
- [x] **5.2.3 `apply_utimens`** — same.
- [x] **5.2.4 `apply_setxattr` (in-inode)** — same.
- [ ] **5.2.5 `apply_setxattr` (external block, depends on Phase 3)** —
  still does direct `dev.write_at` for the xattr block itself; needs a
  multi-block transaction (xattr block + inode + bitmap + BGD + SB).
  Inode write on the same op IS journaled via `bump_inode_ctime`.
- [x] **5.2.6 `apply_truncate_shrink`** — refactored as a multi-block
  journaled transaction. New `BlockBuffer` type accumulates inode +
  bitmap + BGD + SB mutations via `buffer_*` helpers
  (`buffer_free_block_run_and_bgd`, `buffer_patch_bgd_counters`,
  `buffer_patch_sb_counters`, `buffer_write_inode`). The whole buffer
  commits atomically through `commit_block_buffer` (journal when
  available, direct writes otherwise). Pinned by
  `tests/journal_writer_truncate_shrink.rs` including a 0..=30 budget
  sweep that proves crash atomicity (size is always either original
  or target, never torn).
- [x] **5.2.7 `apply_truncate_grow`** — same `commit_inode_write` route.

All four single-inode ops (chmod, chown, utimens, in-inode setxattr,
truncate_grow) pinned by `tests/journal_writer_inode_ops.rs` which
asserts each one advances `jsb.sequence` in production, AND that the
journal self-checkpoints back to clean.
- [ ] **5.2.8 `apply_create`** — needs buffer-aware refactor of the
  `add_dir_entry` / `extend_dir_and_add_entry` helpers first.
- [x] **5.2.9 `apply_unlink`** — multi-block journaled. Buffers the
  parent-dir-block edit, target-data-block frees, target-inode-bitmap
  clear, BGD/SB counter updates, and zeroed-inode write into one
  transaction. Pinned by `tests/journal_writer_unlink_rmdir.rs` with
  budget sweep 0..=40.
- [ ] **5.2.10 `apply_rename`**
- [ ] **5.2.11 `apply_link`**
- [ ] **5.2.12 `apply_symlink`**
- [ ] **5.2.13 `apply_mkdir`** — same blocker as `apply_create`.
- [x] **5.2.14 `apply_rmdir`** — multi-block journaled. Buffers
  target-data-block frees, inode-slot free + used_dirs decrement,
  BGD/SB counters, parent-dir-entry removal, parent nlink decrement
  into one transaction.
- [ ] **5.2.15 `apply_replace_file_content`**

Helpers needing a "buffer-twin" before the rest of 5.2 can ship
journaled: `add_dir_entry`, `extend_dir_and_add_entry`,
`extend_dir_and_add_entry_depth1`, `mark_inode_used` (already done),
`set_block_run_used` (mostly done — see `buffer_mark_block_run_used`).

### 5.3 Journal modes

- [ ] **5.3.1 data=ordered (default)** — metadata journaled, data
  written to final location before commit.
- [ ] **5.3.2 data=writeback (opt-in)** — metadata journaled, data
  unordered. Faster, less safe.
- [ ] **5.3.3 data=journal (opt-in)** — both journaled. Simplest
  crash-safety story; useful for fault-injection tests.

Acceptance: power-fail simulator interrupts each op at every block
write; replay yields a consistent fs (verified by reading + by
external `e2fsck -fn` when available).

---

## Phase 6 — Orphan List & Recovery

- [ ] **6.1 Orphan-list parsing** — read `s_last_orphan` chain at mount.
- [ ] **6.2 Orphan replay** — for each orphan inode, free its blocks
  + inode (under a recovery transaction).
- [ ] **6.3 Orphan-list inserts** — when unlinking a still-open inode,
  insert at head; when closing, remove. (Driver doesn't track open
  fds today; may stub until FSKit/FUSE layer wires it through.)
- [ ] **6.4 Link-count audit** — extend `verify_link_counts`
  (planned in IMPROVEMENT-PLAN B2) to actually fix discrepancies under
  a recovery transaction, not just report.

Acceptance: image with manually-inserted orphan inodes mounts clean,
orphans freed, no fsck warnings.

---

## Phase 7 — Stability Hardening

Inherits from `IMPROVEMENT-PLAN.md` Phase A. Listed here so a single
checklist tracks "production-ready" status.

- [~] **7.1 Purge `.unwrap()` from parse paths** (A1) — empirically
  the existing 213 `.unwrap()` calls are after-bounds-check (the
  caller validates `buf.len() >= N` before slicing). The fuzz harness
  (10 read-side tests + 6 write-side tests) finds zero panics on
  truncated/zeroed/byte-flipped/random-bit-flipped images. Mass
  cosmetic refactor deferred — the contract holds without it.
- [ ] **7.2 Checked arithmetic** (A2) — only the known hot sites in
  IMPROVEMENT-PLAN are still raw multiplies; rare in practice.
- [ ] **7.3 FFI input validation** (A3) — partially done (NUL/empty
  rejection in capi); a full sweep is deferred.
- [ ] **7.4 Richer error variants** (A4) — cosmetic; `Corrupt(&str)`
  carries enough context for now.
- [x] **7.5 Malformed-image fuzz harness** (D1) — 10 read-side tests
  in `tests/fuzz_smoke.rs` (truncated images, zero-fill, byte flips,
  PRNG inputs, exhaustive-bit-flip on first sector) PLUS 6 write-side
  tests in `tests/fuzz_write_paths.rs` (stomped inode tables / block
  bitmaps, extreme setxattr values, extreme truncate sizes,
  writes-to-nonexistent-paths, RO-device write rejection). All green.

---

## Phase 8 — Performance

Also from `IMPROVEMENT-PLAN.md`. Required before claiming "fast" but
not required for correctness.

- [x] **8.1 LRU block cache** (C1) — new `src/block_cache.rs`:
  `CachedDevice` wrapper around any `BlockDevice`. Block-aligned
  single-block reads cache; multi-block reads bypass; writes
  invalidate. Hand-rolled LRU keyed on (block, recency-seq) — no
  external crate. Pinned by 5 unit tests + integration test in
  `tests/block_cache_integration.rs` showing 5.5× reduction in
  inner-device reads on a real-fs workload.
- [ ] **8.2 Extent lookup memoization** (C2) — already partly done by
  `cached_extent` per-call in `file_io::read`; cross-call memo deferred.
- [x] **8.3 Bitmap scan vectorization** (C3) — `find_first_free` was
  already u64-stride-vectorized. `find_free_run` now leverages it to
  skip large used regions instead of bit-at-a-time scanning. The
  run-length verification still walks bit-by-bit (almost always
  short).
- [ ] **8.4 Writeback batching** — most ops already submit one
  multi-block transaction (BlockBuffer pattern from Phase 5.2);
  remaining win is coalescing adjacent dirty blocks within the
  buffer. Deferred.

---

## Phase 9 — Optional / Compat Features

Order roughly by external demand. None block correctness for the
common case but each broadens the image-set we can mount-and-modify.

- [ ] **9.1 Indirect-block (legacy ext3) directory support** —
  read-side first, then write. `capi.rs:923`.
- [ ] **9.2 Indirect-block (legacy ext3) data extents** — read + write
  for files in images created without `extents` feature.
- [ ] **9.3 Casefold (`EXT4_FEATURE_INCOMPAT_CASEFOLD`)** — hash impl
  exists in `casefold.rs`; wire into HTree lookups + dir-entry
  comparisons.
- [ ] **9.4 Project quota** — read project IDs from xattr, enforce on
  write.
- [ ] **9.5 Disk quota (user/group)** — `aquota.user` / `aquota.group`
  parsing + enforcement.
- [ ] **9.6 fs-verity** — Merkle-tree verification on read; immutable
  semantics on write.
- [ ] **9.7 fscrypt v2** — per-file/per-dir encryption. Large surface;
  blocks on userspace key-management contract.
- [ ] **9.8 Online resize (`resize_inode`)** — grow filesystem to fill
  larger backing device.
- [ ] **9.9 mmap shared writes** — coherence with page cache; depends
  on FSKit/FUSE host integration.

---

## Test Matrix

Every phase ships a regression test under `tests/`. Naming:
`tests/phase{N}_{feature}.rs`. CI gate: all tests pass against the
synthetic image set in `test-disks/`. Acceptance for a phase is
"all phase tests green AND prior-phase tests still green".

When `e2fsck` is available on `$PATH`, post-mutation tests invoke
`e2fsck -fn` and assert no warnings (skip otherwise — never make it
a hard dep, since it's a non-shippable tool for our binary).

## Execution Order

Working order is small-and-isolated first to establish the journaling
pattern before tackling the big refactors:

1. **Phase 1** (counter consistency) — unblocks reliable Phase 2/3.
2. **Phase 2.1** (sparse-grow) — single-function win, immediate user
   value (truncate-up actually works).
3. **Phase 3.2 + 3.3** (xattr block alloc) — isolated; closes a
   common ENOSPC.
4. **Phase 5.1** (journal write infra) — the big lift.
5. **Phase 5.2.1 → 5.2.15** (op-by-op wrap) — incremental, each
   step shippable.
6. **Phase 4** (depth ≥ 2 extents) — needed for large fragmented
   files.
7. **Phase 6** (orphans) — finishes the crash-safety story.
8. **Phase 7 + 8** (stability + perf) — interleave throughout; these
   are not blockers.
9. **Phase 9** (optional) — pulled in by user demand.
