# Test disks

Images under `test-disks/` exercise specific ext4 features. Each image has
a sibling `.meta.txt` that documents its structure, so the fixtures are
self-describing — no external key needed.

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
