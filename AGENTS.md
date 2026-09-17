# Working in rust-fs-ext4 (agent guide)

Pure-Rust ext2/3/4 driver (`fs-ext4`) exposing a C ABI (`fs_ext4_*`), consumed
by the DiskJockey FSKit extension. This file is the fast path for an agent
adding or fixing functionality, so you don't re-derive the workflow each time.
It points at the existing docs rather than duplicating them:

- **README** → `## Test contract` (suite shape), `## Building`, `### Testing`.
- **docs/TEST-DISKS.md** → the `test-disks/*.img` fixtures + their `.meta.txt`.

## Skills to use

- **`dev-loop`** — the required loop for any non-trivial change: baseline the
  full suite → change → re-run (no baseline test may regress) → enhance tests →
  vet. Always run it.
- **`commit`** / **`pr`** — for grouping commits and opening PRs. Branches are
  `cth/<name>`; commit subject + flat one-sentence bullets; **no AI attribution**.
- Discipline for **bug fixes**: **prove it's broken first** (a failing
  check/test), *then* fix, *then* prove the same check is green, *then* confirm
  the full baseline still passes. Never write the fix before you have a red.

## Running tests

The tasks are the interface; CI runs exactly these (`chores.yml` has the
contract at its top):

```sh
chore siblings        # ../rust-fs-core and ../fs-linux-test-harness at their pinned refs
chore tools           # install + verify the host oracle tools (e2fsprogs)
chore fixtures        # build test-disks/*.img in the fs-linux-test-harness VM
chore test:unit       # no tools, no fixtures (debug; traps overflows)
chore test:oracle     # the tests that run e2fsck / debugfs / mkfs on the host
chore test            # everything, as CI runs it
chore lint            # fmt + clippy -D warnings

./scripts/test.sh --test <name>   # one integration binary, e.g. repro_wants_dir_symlinks
```

**Nothing skips.** A test that needs a fixture gets it from
`fs_ext4_test_support::fixture` and one that needs a tool from
`oracle_tool` / `assert_e2fsck_clean`; both fail, naming `chore fixtures` or
`chore tools`, when it is missing. Never add an early return for a missing
image or tool: a skipped test reads exactly like a passing one.
`scripts/test-targets.sh` derives the unit and oracle tiers from those
calls; a library test that needs a fixture or a tool goes in a `needs_host`
module so `chore test:unit` can leave it to `chore test`.

Use `scripts/test.sh` for local runs: it selects a platform-aware, per-run
scratch directory and cleans it afterward. In particular, Raspberry Pi runs
use this worktree's `./tmp` (normally the NVMe checkout) rather than the SD
card-backed system temporary directory. GitHub Actions uses `RUNNER_TEMP` when
available; macOS and ordinary hosts use their environment-provided temporary root.
Override the managed base with `FS_EXT4_TEST_TMP_BASE`, or provide a
caller-managed exact directory with `FS_EXT4_TEST_TMPDIR`.

Install the hooks once per clone: `./scripts/install-hooks.sh` (runs
`cargo fmt --check` + `cargo clippy -D warnings` on every commit).

## Adding a test (the in-tree pattern)

Integration tests copy a fixture, drive the driver via `apply_*`, then reopen
read-only and assert. Canonical templates:
`tests/journal_writer_create_mkdir_link_symlink.rs`,
`tests/repro_wants_dir_symlinks.rs`.

```rust
let path = copy_to_tmp("ext4-csum-seed.img", "tag"); // fixture → unique scratch copy
{
    let dev = FileDevice::open_rw(&path)?;
    let fs = Filesystem::mount(Arc::new(dev))?;           // replays journal if dirty
    fs.apply_mkdir("/d", 0o755)?;
    fs.apply_symlink("../x", "/d/x")?;
    fs.apply_unlink("/d/x")?;
}                                                          // drop → unmount/flush
// reopen read-only and assert: jbd2::read_superblock().is_clean(),
// path::lookup(...), and — for checksum work — recompute & compare the
// on-disk checksum (see assert_jsb_checksum_valid in repro_wants_dir_symlinks.rs).
```

**`is_clean()` is not sufficient on its own** — it only checks `jsb.start == 0`.
A bad-but-marked-clean checksum passes it. For checksum bugs, either recompute
the specific checksum in-process (jsb example above) or cross-check with a real
ext4 (below).

## The oracles: e2fsprogs on the host, the kernel in the harness VM

The driver shares this crate's spec interpretation, so its own
`verify::verify` / `fsck::audit` (structural: link counts, dirents, free-count
drift) **cannot** catch metadata_csum / journal-checksum / `itable_unused` bugs,
and no reader of ours can prove the bytes it wrote are the bytes on disk.
Independent tools can:

- **`e2fsck -fn` on the host** — consistency. `assert_e2fsck_clean(image, tag)`
  in the test support crate. Exit `0` clean, `4` errors left uncorrected, `8`
  operational error, `12` cannot proceed (e.g. a corrupt journal superblock).
- **`debugfs` on the host** — content and metadata: `dump` + compare,
  `stat`, `ex`, `icheck`, `ncheck`, `logdump`. `tests/oracle_debugfs.rs` is the
  template, including the negative case that proves why both are needed (a
  flipped data byte passes e2fsck and fails the dump comparison).
- **The kernel**, for what only it can do: `chore fixtures` populates the
  fixtures through the in-kernel ext4 driver (loop mounts, xattrs, ACLs,
  inline data, htree directories) inside the
  [fs-linux-test-harness](https://github.com/antimatter-studios/fs-linux-test-harness)
  VM. The recipes are `test-disks/guest-build-images.sh` (runs as root in the
  guest) and `scripts/vm-setup.sh` (the guest's tooling). For anything else in
  the guest: `chore vm:run -- <command>`, `chore vm:put <file>` (lands in
  `/share`), `chore vm:down`. One VM runs at a time across every repository on
  the machine; `chore vm:slot:status` says who has it.
- `scripts/cross-validate-lwext4.sh` + `tests/lwext4_cross_validate.rs` —
  an independent C implementation; **not implemented yet** (#99), so its one
  test is `#[ignore]`d and fails when run.
- `tests/{qemu,vagrant}/freebsd/` — a real kernel, but FreeBSD's ext4 validates
  JBD2 differently from Linux; not pre-built.

To check a driver-mutated image by hand: `e2fsck -fn <image>` and
`debugfs -R 'stat /path' <image>` on the host (after `chore tools`).

## Build environment

- The build cache (`target/`) is large. If the disk is tight, set
  `CARGO_TARGET_DIR` to a roomier volume — do **not** clean unrelated projects'
  `target/` dirs.
- Toolchain is pinned (`rust-toolchain.toml`); the hook enforces
  `clippy -D warnings` (e.g. `manual_div_ceil` → use `.div_ceil(n)`).

## Worked example: the metadata_csum write-path fixes

A real bug surfaced from the field (a Bookworm SD card whose journal the kernel
rejected after a write). Reproduced with `tests/repro_wants_dir_symlinks.rs` on
`ext4-csum-seed.img`, then fixed as a stack, each step proven red→green with
a real `e2fsck` and the full baseline:

1. **jbd2 superblock checksum** not recomputed in `journal_writer::write_jsb`.
2. **`bg_itable_unused`** not maintained on inode alloc (`buffer_mark_inode_used`).
3. **inode/block bitmap checksums** not recomputed on bitmap change
   (`buffer_refresh_bitmap_csum` + the four bitmap-mutating ops).
4. **inode checksum** not `i_extra_isize`-aware in `compute_inode_checksum`
   (zeroed/freed inodes need the 16-bit lo-only form).

The pattern to copy: `metadata_csum` writes must recompute **every** affected
checksum (superblock, group descriptor, bitmaps, inode, journal) — and the only
reliable proof is a real `e2fsck`, not the driver's own readers.
