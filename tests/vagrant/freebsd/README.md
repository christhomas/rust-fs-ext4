# FreeBSD cross-validator

Independent oracle for fs-ext4 images, using FreeBSD's native (BSD-licensed)
`mount_ext2fs(8)` driver. The companion to `lwext4` cross-validation —
together they form a three-way oracle (kernel + lwext4 + FreeBSD) that
catches spec-ambiguity-driven defects no single implementation would notice.

## Why FreeBSD's driver

- BSD-2-Clause licensed (same family as lwext4) — fits our no-GPL constraint.
- Independent code lineage from both Linux's ext4 driver and lwext4.
- `mount_ext2fs` ships in FreeBSD's base system — zero install steps.
- Handles ext2 + ext3 read-write, ext4 read-only.

## Setup (host machine)

Requires Vagrant + a hypervisor (VirtualBox on Linux/x86_64, UTM on Apple
Silicon — see Vagrantfile header for arm64-host caveats):

```sh
cd tests/vagrant/freebsd
vagrant up        # ~2 min first time (downloads box, ~500 MiB)
```

## Running the validator

```sh
vagrant ssh -c /vagrant/run-cross-validate.sh
```

Output: per-image manifest in `/tmp/freebsd-cross-manifest/<image>.manifest`
inside the VM. Each line is `tab-separated: relpath, size, mode, sha256`.

To diff against the host (full integration is future work — same
incremental approach as the lwext4 stub in `tests/lwext4_cross_validate.rs`):

```sh
# inside VM
vagrant ssh -c "cat /tmp/freebsd-cross-manifest/ext4-basic.manifest" \
    > host-manifest-from-freebsd.txt
# host: produce equivalent manifest via fs_ext4::Filesystem::mount + walk
# diff the two
```

## Teardown

```sh
vagrant halt        # power off (resumable)
vagrant destroy     # full removal (~500 MiB reclaimed)
```

## Phase A status

The Vagrantfile, validator script, and manifest format are in place.
Wiring the host-side diff harness into `cargo test` is intentionally
left for follow-up — same opt-in pattern as
`tests/lwext4_cross_validate.rs`. Run manually before each release;
add to a dedicated CI lane when there's appetite.
