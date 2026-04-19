# ext4rs

Pure-Rust ext4 filesystem driver. Exposes a stable C ABI (`fs_ext4_*`)
designed to be linked from Swift, C, C++, Go (via CGo), or any other
language with FFI.

Built to back [DiskJockey](https://github.com/christhomas/diskjockey)'s
macOS FSKit extension for ext4 mounts, but has no macOS or FSKit
dependency itself — the library is portable Rust.

## Status

| Capability | Status |
|---|---|
| Mount ext4 image or block device | done |
| stat, readdir, read file | done |
| readlink, listxattr, getxattr | done |
| inline data, htree directories | done |
| extent trees (leaf + single-level internal) | done |
| checksum verification (metadata_csum, csum_seed) | done |
| create, unlink, mkdir, rmdir | done |
| rename (no-clobber, POSIX `EINVAL` into own subtree) | done |
| hardlink, truncate (shrink), write file (replace body) | done |
| multi-level extent tree mutation (depth 0→1 promotion + depth-1 inserts) | done |
| multi-level extent tree mutation (depth ≥2, leaf-block split) | **not supported** |
| sparse grow via truncate (i_size bump, reads return zeros) | done |
| setxattr, removexattr (in-inode) | done (via `fs_ext4_setxattr` / `fs_ext4_removexattr`) |
| setxattr/removexattr on external xattr block | **not supported** |
| chmod, chown, utimens | done (via `fs_ext4_chmod` / `fs_ext4_chown` / `fs_ext4_utimens`) |
| journaled transactions | partial (jbd2 replay; write path unjournaled) |
| read-only fsck audit (link counts, dangling entries) | done (via `Filesystem::audit`) |
| opt-in LRU block cache (for remote callback devices) | done (via `CachingDevice`) |

Roughly a read/write driver for the common case. Directories that have
been promoted to depth 1 can keep growing up to their leaf block's
capacity (~340 extents on a 4 KiB block); beyond that, or for sparse-
file extension, are the known gaps. POSIX errnos are mapped through
(`ENOENT`, `ENOTDIR`, `EISDIR`, `EINVAL`, `EEXIST`, `ENOTEMPTY`,
`EROFS`, `ENAMETOOLONG`, `ENOTSUP`).

## Building

```sh
cargo build --release
# produces target/release/libfs_ext4.a and the rlib
```

Cross-compile to a specific target the usual way:

```sh
cargo build --release --target aarch64-unknown-linux-gnu
cargo build --release --target x86_64-pc-windows-gnu
# … etc.
```

Platform-specific packaging (e.g. macOS `lipo` to build a universal
static archive, producing an Xcode `.xcframework`, or cross-compile
matrices for a particular SDK) belongs in the consuming project.
`ext4rs` itself stays portable cargo — it does not carry platform-
specific build scripts.

## Using from C

Link `libfs_ext4.a` and include `fs_ext4.h`:

```c
#include "fs_ext4.h"

fs_ext4_fs_t *fs = fs_ext4_mount("/path/to/disk.img");
if (!fs) {
    fprintf(stderr, "%s\n", fs_ext4_last_error());
    return 1;
}

fs_ext4_attr_t attr;
if (fs_ext4_stat(fs, "/hello.txt", &attr) == 0) {
    printf("size=%llu mode=%o\n", attr.size, attr.mode);
}

fs_ext4_umount(fs);
```

See `examples/capi_demo.rs` for the Rust-side equivalent.

## Using from Rust

```toml
[dependencies]
ext4rs = "0.1"
```

```rust
use fs_ext4::Filesystem;

let fs = Filesystem::mount("/path/to/disk.img")?;
let attrs = fs.stat("/hello.txt")?;
```

## Testing

```sh
cargo test --release
```

Integration tests use ext4 image fixtures under `test-disks/`.
The fixtures are gitignored — regenerate them with:

```sh
bash test-disks/build-ext4-feature-images.sh
```

The generator runs standard formatter tools inside a
short-lived Alpine Linux VM booted under `qemu-system-x86_64`, so
the same script works on macOS, Linux, and in CI (no docker
required). First run downloads the Alpine virt ISO + kernel (~75 MB,
cached under `test-disks/.vm-cache/`).

## License

MIT — see [LICENSE](LICENSE). Derives research from
[yuoo655/ext4_rs](https://github.com/yuoo655/ext4_rs) (MIT) and
[lwext4](https://github.com/gkostka/lwext4) (BSD).
