# Test disks

Images under `test-disks/` exercise specific ext4 features. Each image has
a sibling `.meta.txt` that documents its structure, so the fixtures are
self-describing — no external key needed.

Build them with `chore fixtures`. They need the real kernel's ext4 driver to
populate (each image is formatted, loop-mounted and written through the
kernel), so the recipes in `test-disks/guest-build-images.sh` run as root in
the [fs-linux-test-harness](https://github.com/antimatter-studios/fs-linux-test-harness)
VM, and `test-disks/build-fixtures.sh` moves the finished images into
`test-disks/`. The same path runs everywhere: KVM on Linux, HVF on macOS, and
the `fixtures` CI job, which hands the images to the test jobs on both
architectures as an artifact. `test-disks/build-fixtures.sh htree xattr`
rebuilds only some; `test-disks/build-fixtures.sh --check` lists what is
missing.

Every mkfs pins its UUID and directory hash seed, so each build has the same
layout and checksum seeds; kernel-stamped times still differ between builds.
A test never skips on a missing image: it fails naming `chore fixtures`.

| Image | Exercises |
|---|---|
| `ext4-basic.img` | minimal extent + dir entries |
| `ext4-htree.img` | hashed directory |
| `ext4-inline.img` | inline_data feature |
| `ext4-xattr.img` | xattr reads |
| `ext4-deep-extents.img` | multi-extent files |
| `ext4-csum-seed.img` | metadata_csum with csum_seed |

For each image: open read-only, walk the directory tree, read the files
named in `<image>.meta.txt`, and confirm the content matches. That is the
smoke test the driver is expected to pass.
