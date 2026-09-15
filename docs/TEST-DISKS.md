# Test disks

Images under `test-disks/` exercise specific ext4 features. Each image has
a sibling `.meta.txt` that documents its structure, so the fixtures are
self-describing — no external key needed.

Regenerate them with `bash test-disks/build-ext4-feature-images.sh`. The
short-lived Alpine Linux oracle follows the local host architecture by
default: x86_64 on Intel/AMD and aarch64 on Apple Silicon/ARM Linux. Use
KVM on Linux or HVF on macOS; the script fails clearly when native hardware
acceleration is unavailable. `EXT4_VM_ARCH=x86_64` or `EXT4_VM_ARCH=aarch64`
with `EXT4_VM_ALLOW_TCG=1` is the explicit, slow cross-architecture fallback.
VM downloads live under `.vm-cache/` in architecture-specific paths, so one
host cannot reuse another architecture's kernel, initramfs, ISO, or APKs.

GitHub's x86_64 and ARM64 jobs are themselves real Linux environments. They
run `sudo bash test-disks/build-ext4-feature-images-native-linux.sh` so fixture
generation uses the native kernel and external `e2fsprogs` tools without
depending on nested virtualisation.

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
