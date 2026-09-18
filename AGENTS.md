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
chore tools           # what the HOST needs: ripgrep, and the VM (Vagrant/QEMU/KVM)
chore fixtures        # build test-disks/*.img in the fs-linux-test-harness VM
chore test:unit       # no tools, no fixtures, no VM (debug; traps overflows)
chore test:images     # reads a fixture, needs no VM
chore test:oracle     # e2fsck / debugfs / mke2fs — run INSIDE the VM
chore test:kernel     # the real kernel loop-mounts our images and reads them back
chore test:vm         # the whole suite, compiled and run INSIDE the VM
chore test            # everything, as CI runs it
chore lint            # fmt + clippy -D warnings

./scripts/test.sh --test <name>   # one integration binary, e.g. repro_wants_dir_symlinks
```

**The oracle tools are never run on the host.** Not on a Mac, not on
Linux, not even where they are installed. e2fsprogs on a workstation is
whatever that machine has, and an oracle whose answer depends on the
machine is not an oracle — so they live in one Debian guest
(`scripts/vm-setup.sh` installs them) and `fs_ext4_test_support::oracle`
is the only way in. `tests/test_contract.rs` fails the suite if a test
spawns one itself, drives the VM itself, or mounts anything on the host.
The VM boots once per run (the first oracle call), every later call rides
one shared SSH connection (~0.1 s per tool invocation), and the chore
reaper stops it when the invocation ends.

**We run the Linux tests on Linux.** On a Linux host `chore test` runs
them here; on macOS it runs `chore test:vm`, which builds and runs the
same sources inside the guest (the harness mounts the repository there at
the path the host knows it by). Nothing Linux-shaped runs natively on a
Mac.

**Nothing skips.** A test that needs a fixture gets it from
`fs_ext4_test_support::fixture`, one that needs a tool from `oracle` /
`assert_e2fsck_clean`, one that needs the kernel from the guest-kernel
helpers; all of them fail, naming the task that provides it, when it is
missing. Never add an early return for a missing image, tool or VM: a
skipped test reads exactly like a passing one.
`scripts/test-targets.sh` derives the tiers from those calls; a library
test that needs a fixture or the VM goes in a `needs_host` module so
`chore test:unit` can leave it to `chore test`.

Use `scripts/test.sh` for local runs: it gives every run its own scratch
directory and cleans it afterward. **That directory is inside the
repository** (`./tmp`, gitignored) on every machine, because the guest
sees this repository and nothing else of the host — an image under `/tmp`
or `$RUNNER_TEMP` would not exist for the tool asked to read it.
`FS_EXT4_TEST_TMPDIR` names an exact directory instead, and is refused if
it is outside the repository.

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

## The oracles: e2fsprogs and the kernel, both in the harness VM

The driver shares this crate's spec interpretation, so its own
`verify::verify` / `fsck::audit` (structural: link counts, dirents, free-count
drift) **cannot** catch metadata_csum / journal-checksum / `itable_unused` bugs,
and no reader of ours can prove the bytes it wrote are the bytes on disk.
Independent tools can:

- **`e2fsck -fn`** — consistency. `assert_e2fsck_clean(image, tag)` in the
  test support crate. Exit `0` clean, `4` errors left uncorrected, `8`
  operational error, `12` cannot proceed (e.g. a corrupt journal superblock).
- **`debugfs`** — content and metadata: `dump` + compare, `stat`, `ex`,
  `icheck`, `ncheck`, `logdump`. `tests/oracle_debugfs.rs` is the template,
  including the negative case that proves why both are needed (a flipped
  data byte passes e2fsck and fails the dump comparison).
  Both are reached through `fs_ext4_test_support::oracle(tool)`, which runs
  them in the guest and returns the tool's own `Output` — same exit status,
  same streams, no host path to fall back to.
- **The kernel itself**, through `guest_kernel_report` /
  `guest_kernel_write` in the test support crate: our image is loop-mounted
  in the guest and a script walks it, hashes every file and reads xattrs
  and ACLs back in ONE guest call. `tests/kernel_readback.rs` (Rust API),
  `tests/kernel_readback_capi.rs` (C ABI). This is what catches what
  `e2fsck` cannot — an ACL blob with the wrong version, an extent the
  kernel maps shorter than we wrote it — and it is the only place a
  filesystem is mounted at all.
- **The fixtures** are kernel-made the same way: `chore fixtures` runs
  `test-disks/guest-build-images.sh` as root in the guest (loop mounts,
  xattrs, ACLs, inline data, htree directories) through the
  [fs-linux-test-harness](https://github.com/antimatter-studios/fs-linux-test-harness)
  VM, whose tooling `scripts/vm-setup.sh` installs. For anything else in
  the guest: `chore vm:run -- <command>`, `chore vm:exec -- <command>` (the
  fast path, for a VM that is already up), `chore vm:put <file>` (lands in
  `/share`), `chore vm:down`. One VM runs at a time across every repository
  on the machine; `chore vm:slot:status` says who has it.
- `scripts/cross-validate-lwext4.sh` + `tests/lwext4_cross_validate.rs` —
  an independent C implementation; **not implemented yet** (#99), so its one
  test is `#[ignore]`d and fails when run.
- `tests/{qemu,vagrant}/freebsd/` — a real kernel, but FreeBSD's ext4 validates
  JBD2 differently from Linux; not pre-built.

To check a driver-mutated image by hand, ask the guest — the tools are
there, and it sees this repository at the same path the host does:

```sh
chore vm:up                                   # boot and hold it
chore vm:run -- e2fsck -fn "$PWD/tmp/x.img"
chore vm:run -- debugfs -R "'stat /path'" "$PWD/tmp/x.img"
chore vm:down
```

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
