# Direct-qemu FreeBSD cross-validator

Boots an arm64 FreeBSD VM under qemu (HVF-accelerated on Apple Silicon —
no CPU emulation overhead) and uses its native `mount_ext2fs(8)` driver
to cross-validate fs-ext4 images. Sibling of `tests/vagrant/freebsd/`
(which couldn't escape the Vagrant box-format compatibility lottery —
no FreeBSD box on Vagrant Cloud ships a `qemu`-format build).

## Why not Vagrant

Probed during the May 2026 cross-validator run:

- `bento/freebsd-14`: vmware/parallels/virtualbox only. Apple Silicon
  needs a paid hypervisor or a working VMware Vagrant Utility daemon.
- `generic/freebsd14`: same deal — no qemu provider build.
- `vagrant-qemu-christhomas` plugin: works with custom-built `qemu`-format
  boxes but doesn't transparently consume libvirt-format ones.

Going direct removes one whole layer of "does this provider work here".

## Why not lwext4

`scripts/cross-validate-lwext4.sh` is the *first* independent oracle —
small (~6 MB BSD-2-Clause C library), fast (~30 s build). FreeBSD's
in-kernel ext2/3 driver is the *second*, with a different code lineage
(BSD heritage vs lwext4's clean-room C). Bugs both implementations
share are very likely spec-defined; bugs unique to one are likely
impl defects. Both running clean against the same image is the
strongest correctness signal short of mounting on Linux (which fails
the no-GPL constraint for our test infra).

## Files

- `run-cross-validate.sh` — main entry point. Downloads, builds seed
  ISO, boots qemu, parses serial log into per-image manifests.
- `user-data` — cloud-init NoCloud user-data. Mounts every secondary
  virtio-blk via `mount_ext2fs`, walks files, prints `<rel>\t<size>\t<sha256>`
  bracketed by `[manifest:disk:<name>:begin/end]` markers on serial,
  then `poweroff`.
- `meta-data` — minimal cloud-init metadata (instance-id + hostname).

## Prereqs

- `qemu-system-aarch64` + `qemu-img` (brew install qemu)
- `xorriso` (brew install xorriso)
- `curl`, `xz`
- ~600 MB free disk for the cached image + manifests
- ~2 GiB RAM for the VM

## Running

```sh
./run-cross-validate.sh
```

First run downloads the FreeBSD image (~500 MB). Subsequent runs reuse
the cached image. The cached image lives at `<crate>/.qemu/` — both
`.qemu/` and `.lwext4/` are `.gitignore`'d.

## Status — known blocker (May 2026)

The script fully boots the FreeBSD BASIC-CLOUDINIT-ufs image under
qemu. **However, cloud-init is not auto-starting in the shipped image
as of FreeBSD 14.3-RELEASE.** The boot sequence runs `freebsd-update`
(which downloads patches + requests a reboot) but never invokes any
`cloudinit_*` rc service. The VM powers off with `freebsd-update`
having patched a couple hundred files, but the user-data script
never executes — the manifest output the host parser expects
isn't there.

Three paths forward for future work, in order of effort:

1. **SSH-based driver** (cleanest). Boot qemu in the background
   with `-nic user,hostfwd=tcp::2222-:22`, wait for sshd to come
   up, ssh in with default credentials (need to confirm what they
   are for the BASIC-CLOUDINIT image — possibly `freebsd:freebsd`
   or the password reset by cloud-init that didn't run), execute
   the validator commands over SSH, halt. ~2 hours.

2. **Image surgery on first download**. Modify the qcow2 to enable
   `cloudinit_enable="YES"` in `/etc/rc.conf` before first boot.
   Requires `libguestfs` or `qemu-nbd` (neither available on macOS
   without container tricks). ~3 hours including the container setup.

3. **mfsBSD switch**. Drop FreeBSD's official cloud image entirely
   and use mfsBSD — a tiny memory-resident FreeBSD live system with
   SSH enabled by default. ~1 hour but mfsBSD's release cadence is
   slow; we'd pin to whichever 14.x mfsBSD build exists.

What works today regardless of cloud-init:
- The download + decompression pipeline.
- The qemu invocation (HVF accel, virtio-blk attach for test images,
  EDK2 UEFI boot, serial→file capture).
- The serial-log parser (it correctly returns "didn't reach
  manifest:end" instead of producing garbage).
- The seed ISO build via xorriso.

## Spec sources

- FreeBSD VM-IMAGES distribution layout: download.freebsd.org docs.
- cloud-init NoCloud datasource: cloudinit.readthedocs.io.
- virtio-blk device naming on FreeBSD: `/dev/vd[a-z]` for paravirt
  block devices; `vda` is the boot disk, `vdb`+ are secondary.
