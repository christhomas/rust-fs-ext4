# What a read costs

Measured by `tests/read_path_cost.rs` (#68). Run it with

```sh
cargo test --release --test read_path_cost -- --nocapture
```

It needs `mkfs.ext4` and `e2fsck`, and builds its own tree to a fixed recipe:

- ten directories of 200 files each, 0 to 20 KB, plus three levels of nesting in each;
- one directory of 3000 names, indexed by `e2fsck -fyD`;
- a 128 MiB image at 4 KiB blocks.

The counter is `am-fs-core`'s `CountingDevice`, below the buffer cache, so it counts what reached the device, not what the driver asked for. It matches the sibling drivers' `read_path_cost`. Every figure is calls to `read_at` and the bytes they asked for. Wall time is printed but isn't the point.

## Figures

e2fsprogs 1.47.0, `aarch64`, 2026-09-17. `mkfs.ext4 -d` copies the tree in the order the host lists it, so inode placement, and with it a few calls, moves between builds. The `stat` figure at the default capacity came out at 338 on one build and 345 on the next. Compare shapes and orders of magnitude, not the last digit.

| shape | items | no clean cache | 256 blocks (default) | 1024 blocks |
|---|---:|---:|---:|---:|
| mount | 1 | 4 calls, 9 KB | 4 calls, 9 KB | 4 calls, 9 KB |
| walk (list every directory) | 33 dirs | 5130 calls, 21.0 MB | 370 calls, 1.5 MB | 370 calls, 1.5 MB |
| stat (resolve every path, read its inode) | 5042 paths | 28246 calls, 115.7 MB | 345 calls, 1.4 MB | 0 calls |
| read (every regular file) | 5010 files | 10922 calls, 44.7 MB | 6227 calls, 25.5 MB | 6212 calls, 25.4 MB |

The first pass mounts with `Filesystem::mount_with_cache(dev, 0)`. That keeps no clean blocks, but it still reads whole blocks, which is why every call there is 4 KiB.

## What the two open questions come to

**Capacity.** The default 256 blocks turns a walk from 5130 calls into 370, and resolving 5042 paths from 28246 calls into 345. At 1024 blocks the resolution makes no calls at all, because every directory and inode-table block it touches fits. The walk stays at 370 either way: that's the first sight of each directory, and no cache can remove it. File reads barely move at either size, because the data is larger than the cache and is read once each. So 256 is enough to make metadata cheap on a tree this size. Four times as much makes repeated path resolution free, and does nothing for data.

**Multi-block reads bypass the cache.** In every pass the bytes divided by the calls is exactly 4096. No read that reached the device spanned more than one block, in any of the four shapes. The bypass exists, but this tree doesn't exercise it: directory, inode and file reads all go one block at a time.

## Assertions

The test asserts structure, not these numbers, since the numbers move with the build:
- the uncached pass reaches the device in every shape;
- the cached passes do the same work;
- a cache never needs more calls than no cache;
- a larger cache never needs more calls than a smaller one. An LRU cache is a stack algorithm, so this must hold.
