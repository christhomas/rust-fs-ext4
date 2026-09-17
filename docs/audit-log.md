# Write-path audit log

Every pass over this driver's write paths asks one question:

> Is there an edit that can be applied to a filesystem the driver has **misread**?

The question is not about a missing bounds check. It's about a value read one way and then used another, on an ordinary filesystem. That's the shape of every corruption defect found in this family of drivers so far. The same log, in the same format, is kept in `rust-fs-ext4`, `rust-fs-xfs`, `rust-fs-btrfs` and `rust-fs-ntfs`.

Each pass is one entry, newest first. An entry names:
- the commit it read;
- every module it looked at, with the result, "none found" included;
- each finding with its issue;
- what it didn't reach, so the next pass knows where to start.

## 2026-09-17: file data, extent-tree, xattr-block, name and block-map paths

Read at `b7395bc` (#70).

| Area (`src/fs.rs` unless named) | Result |
|---|---|
| `apply_pwrite` | #240 |
| `apply_fallocate_keep_size` | same blind spot as #240, caught by the overlap refusal |
| `apply_fallocate_punch_hole`, `apply_fallocate_zero_range` | #242 |
| `apply_unlink`, `apply_replace_file_content`, `apply_rename` (replacing), `apply_rmdir`, orphan release | #242 |
| `apply_truncate_shrink`, `file_mut::plan_truncate_shrink` | none found: extents past EOF are freed, and deeper trees are refused |
| `apply_truncate_grow` | none found |
| `extent_mut::plan_insert_extent`, `plan_insert_extent_deep` | none found: both refuse an overlap |
| `apply_setxattr`, `apply_removexattr` (external block), and the xattr block on unlink, rmdir, rename-over and orphan release | #245 |
| `split_parent_and_base`: the names create, mkdir, mknod, symlink, link and rename file | #247 |
| `apply_unlink` and `apply_replace_file_content` on block-mapped files | #249 |
| `buffer_update_dotdot` | none found: `..` is at offset 12 in every directory the kernel or mke2fs writes, and inline-data directories are refused for writes |

**Findings:**
- **#240.** `map_logical` answers `None` for an uninitialized extent, so reads see zeros. `apply_pwrite` took that for a hole, and a write into preallocated space was refused as a corrupt extent tree on a valid volume. Reproduced with `mkfs.ext4`.
- **#242.** Freeing a file read its blocks from `i_size`, not from its tree: an empty file's `KEEP_SIZE` preallocation was dropped from the tree and left allocated. Freeing also took the leaf extents for the whole tree, so a deep tree's node blocks leaked on punch-hole and rmdir, and unlink of such a file was refused. Reproduced; `e2fsck -fn` rejected each result.
- **#245.** An external xattr block was read as the inode's own, but the kernel shares one block among inodes with identical attributes (`h_refcount`). Setting an attribute rewrote the shared block and reset its count to one, removing the last one freed a block others still used, and unlinking never released the block at all. Reproduced on a shared block built as the kernel leaves it.
- **#247.** Names arrive as `&str`, which can hold NUL, and were filed as given. The entry format stores counted bytes, so nothing stopped it, and e2fsck reports it as an illegal character.
- **#249.** Unlink freed blocks only for extent-mapped files, so a block-mapped file's data and indirect blocks leaked. Rewriting one used unbuffered helpers that wrote the block bitmap without restamping its checksum. Reproduced on `-O ^extent,^64bit[,metadata_csum]` volumes.

**Not reached, for the next pass:**
- `src/xattr.rs`'s in-inode region edits, `src/ea_inode.rs`;
- `src/htree_mut.rs` and directory entry edits beyond rmdir;
- `src/indirect_mut.rs` beyond unlink and replace-content: rmdir and rename-over of block-mapped inodes;
- `src/inline_data.rs`;
- `src/journal_writer.rs`, `src/transaction.rs`;
- `src/mkfs.rs`;
- `src/capi.rs`'s entry points beyond what they call.

## Before this log

The pass had run in substance before any record was kept. The findings below each answer this log's question, and all are closed:
- #78: orphan recovery deleted a file that still had directory links.
- #79: orphan recovery freed an ext2/ext3 orphan's inode and leaked its data blocks.
- #83: EA_INODE xattr values were read from the wrong place.
- #84: a casefolded volume mounted writable, and a create filed its entry in the wrong htree leaf.
- #86: `block_group_count` ignored `s_first_data_block`.
- #91: group-descriptor checksums weren't written on a `GDT_CSUM`-only volume.
- #97: a create in an htree directory split the dx_root's fake `..` record.
- #118: freeing a run across a group boundary leaked the second group's blocks.
- #124: unjournaled orphan recovery wrote the superblock first.

Two further fixes came from the same reading: `ab62459` gated writes on the read-only feature rules, and `81fe017` took `GDT_CSUM` out of the maintained set.
