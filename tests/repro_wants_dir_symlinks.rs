//! Repro: the "enable systemd units" symlink edit that corrupted a Raspberry
//! Pi OS (Bookworm) root fs when applied through the DiskJockey ext4 driver.
//!
//! Field op set, verbatim: one `mkdir`, five `symlink` creations, one `unlink`
//! (the pre-existing dangling `inpace.service`). On the real card the kernel
//! then failed to mount with `JBD2: journal checksum error`.
//!
//! Structure mirrors `journal_writer_create_mkdir_link_symlink.rs`: copy a
//! fixture, run the ops through `apply_*`, then reopen read-only and assert the
//! journal returned to clean and every link resolves. First fixture is
//! `ext4-csum-seed.img` (metadata_csum_seed — "the flag that broke lwext4 on
//! the Pi SD card"); `ext4-basic.img` is the control.
//!
//! The mutated image is intentionally left in /tmp (path is printed) so the
//! Alpine-VM `e2fsck` pass can validate it against a real Linux ext4 — the
//! in-process `is_clean()` check cannot catch a bad-checksum-but-marked-clean
//! journal, which is exactly the field symptom.

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, tag: &str) -> Option<String> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!(
        "/tmp/fs_ext4_repro_wants_{}_{tag}_{n}.img",
        std::process::id()
    );
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn assert_clean(path: &str, tag: &str) {
    let dev = FileDevice::open(path).expect("ro reopen");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    if let Some(jsb) = fs_ext4::jbd2::read_superblock(&fs).expect("jsb") {
        assert!(
            jsb.is_clean(),
            "[{tag}] journal NOT clean after the field ops (start={}) — the field bug",
            jsb.start
        );
    }
}

/// The JBD2 superblock checksum on disk must match a fresh recompute after our
/// writes (for checksummed journals). Unlike `is_clean()` — which passed even
/// in the red state — this assertion FAILS when the driver leaves a stale jsb
/// checksum, which is the exact field bug ("Journal superblock is corrupt").
fn assert_jsb_checksum_valid(path: &str, tag: &str) {
    let dev = FileDevice::open(path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let Some(jsb) = fs_ext4::jbd2::read_superblock(&fs).expect("jsb") else {
        return;
    };
    if !jsb.uses_csum_v2_or_v3() {
        return; // v1 journals carry no superblock checksum
    }
    let (jinode, _) = fs
        .read_inode_verified(fs.sb.journal_inode)
        .expect("journal inode");
    let bs = fs.sb.block_size() as u64;
    let phys = fs_ext4::jbd2::journal_block_to_physical(&fs, &jinode, 0)
        .expect("map jsb block")
        .expect("jsb block mapped");
    let mut buf = vec![0u8; bs as usize];
    fs.dev.read_at(phys * bs, &mut buf).expect("read jsb block");

    // s_checksum (0xFC) = crc32c(~0, journal_superblock_t[..1024] with the
    // field zeroed); seeded ~0, independent of the fs metadata_csum seed.
    let stored = u32::from_be_bytes(buf[0xFC..0x100].try_into().unwrap());
    let mut z = buf[..1024].to_vec();
    z[0xFC..0x100].copy_from_slice(&0u32.to_be_bytes());
    let computed = fs_ext4::checksum::linux_crc32c(!0, &z);
    assert_eq!(
        stored, computed,
        "[{tag}] JBD2 superblock checksum stale after writes \
         (stored={stored:#010x} computed={computed:#010x}) — the field bug",
    );
}

/// The four units enabled in the field, plus the dangling `inpace.service`
/// that was removed first.
const UNITS: &[&str] = &[
    "inpace-usbdisk",
    "inpace-app",
    "inpace-wifi",
    "inpace-timesync",
];

fn run_field_ops(img: &str, tag: &str) {
    let Some(path) = copy_to_tmp(img, tag) else {
        eprintln!("[{tag}] fixture {img} missing — skipping");
        return;
    };

    {
        let dev = FileDevice::open_rw(&path).expect("open_rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount (replays if dirty)");

        fs.apply_mkdir("/wants", 0o755).expect("mkdir /wants");
        // the pre-existing dangling link, then the four real ones, then remove it
        fs.apply_symlink("../inpace.service", "/wants/inpace.service")
            .expect("symlink inpace.service");
        for u in UNITS {
            fs.apply_symlink(&format!("../{u}.service"), &format!("/wants/{u}.service"))
                .unwrap_or_else(|e| panic!("[{tag}] symlink {u}: {e:?}"));
        }
        fs.apply_unlink("/wants/inpace.service")
            .expect("unlink inpace.service");
        // fs drops here -> unmount/flush
    }

    assert_clean(&path, tag);
    assert_jsb_checksum_valid(&path, tag);

    // every enabled-unit symlink must still resolve after the remount
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    for u in UNITS {
        let p = format!("/wants/{u}.service");
        fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, &p)
            .unwrap_or_else(|e| panic!("[{tag}] {p} not reachable after ops: {e:?}"));
    }

    eprintln!("[{tag}] mutated image left for real-ext4 check at: {path}");
}

#[test]
fn field_ops_on_csum_seed_keep_journal_consistent() {
    run_field_ops("ext4-csum-seed.img", "csum-seed");
}

#[test]
fn field_ops_on_basic_keep_journal_consistent() {
    run_field_ops("ext4-basic.img", "basic");
}
