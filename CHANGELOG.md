# Changelog

## [Unreleased]

### Build / CI

- Test-disk fixtures now regenerate from scratch on any host with
  `qemu-system-x86_64` + `libarchive-tools` (for `bsdtar`'s
  ISO9660 writer). Drop-in `bash test-disks/build-ext4-feature-images.sh`
  boots a short-lived Alpine Linux VM, runs ext4 formatter + friends
  inside, writes the image matrix out via 9p. Replaces the earlier
  docker-based path so macOS dev hosts don't need Docker Desktop.
  CI (`ubuntu-latest`) runs this before `cargo test`.

## [0.1.0] — 2026-04-18

First public release. Extracted from the internal ext4-fskit research
repo into a standalone crate.

### C ABI — `fs_ext4_*`

- Lifecycle: `fs_ext4_mount`, `fs_ext4_mount_with_callbacks`,
  `fs_ext4_mount_rw`, `fs_ext4_umount`, `fs_ext4_get_volume_info`.
- Metadata: `fs_ext4_stat`, `fs_ext4_last_error`, `fs_ext4_last_errno`.
- Directories: `fs_ext4_dir_open`, `fs_ext4_dir_next`, `fs_ext4_dir_close`.
- Files: `fs_ext4_read_file`, `fs_ext4_readlink`, `fs_ext4_listxattr`,
  `fs_ext4_getxattr`.
- Write ops: `fs_ext4_create`, `fs_ext4_unlink`, `fs_ext4_mkdir`,
  `fs_ext4_rmdir`, `fs_ext4_rename`, `fs_ext4_link`, `fs_ext4_write_file`,
  `fs_ext4_truncate`.

### Driver features

- Multi-level extent tree promotion (depth 0 → depth 1) in
  `extent_mut`, with `Checksummer::patch_extent_tail` so newly
  built leaf blocks carry a valid `ext4_extent_tail.et_checksum`.

### Build / CI

- `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + `cargo
  test --release` on `ubuntu-latest`.
- `CallbackDevice` fields use `ReadCb` / `WriteCb` / `FlushCb` type
  aliases instead of inline `Box<dyn Fn(...) + Send + Sync>`.

### Known gaps

- Multi-level extent tree mutation beyond depth 1 not implemented;
  very large / fragmented writes will fail loudly.
- Sparse grow via truncate not implemented.
- `setxattr`, `removexattr`, `chmod`, `chown`, `utimens` — not in the
  ABI; reads only for xattrs.
- Write path is unjournaled. `jbd2` replay works at mount for a
  cleanly-closed journal; live transactions are not yet wrapped.

### Origin

- Imported from `github.com/christhomas/ext4-fskit@aaa63cf`.
