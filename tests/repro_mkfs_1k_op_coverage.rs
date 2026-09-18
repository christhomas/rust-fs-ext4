//! Wide op-coverage sweep at 1 KiB block size, on images formatted by the
//! driver's own `mkfs` (Ext4 flavor). This covers a corner neither fixture
//! sweep reaches:
//!
//!   * `repro_csum_seed_op_coverage` — metadata_csum WITH a journal, 4 KiB.
//!   * `repro_no_csum_op_coverage`   — no csum, no journal, 4 KiB.
//!   * here                           — metadata_csum WITHOUT a journal, 1 KiB.
//!
//! Ext4 mkfs enables metadata_csum + metadata_csum_seed but lays down no
//! journal, and 1 KiB blocks use the first_data_block=1 layout (see the mkfs
//! bitmap fix). So this stresses the checksum write paths at the small block
//! size, journal-less. Each test formats its own fresh image, and `e2fsck -fn`
//! must pass on it after the ops (`RFE_KEEP_IMAGES=1` keeps it).

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::fs::Filesystem;
use fs_ext4::mkfs;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SIZE: u64 = 8 * 1024 * 1024;
const UUID: [u8; 16] = [
    0x0F, 0x1E, 0x2D, 0x3C, 0x4B, 0x5A, 0x69, 0x78, 0x87, 0x96, 0xA5, 0xB4, 0xC3, 0xD2, 0xE1, 0xF0,
];

/// Format a fresh 8 MiB / 1 KiB-block Ext4 image and return its path.
fn mkfs_1k(tag: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let path =
        fs_ext4_test_support::temp_path!("fs_ext4_mk1kop_{tag}_{}_{n}.img", std::process::id());
    {
        // Image setup must not fail silently: a scratch-disk/permission error here
        // should fail the test loudly, not make the coverage vanish via an
        // early `return` in the callers.
        let f = std::fs::File::create(&path).expect("create 1k image file");
        f.set_len(SIZE).expect("set 1k image length");
    }
    {
        let dev = FileDevice::open_rw(&path).expect("open_rw");
        mkfs::format_filesystem(&dev, Some("MK1KOP"), Some(UUID), SIZE, 1024)
            .expect("format_filesystem 1k");
        dev.flush().expect("flush");
    }
    path
}

fn rw(path: &str) -> Filesystem {
    Filesystem::mount(Arc::new(FileDevice::open_rw(path).expect("open_rw"))).expect("mount")
}

fn done(path: &str, tag: &str) {
    // Re-mount read-only to confirm the image still mounts after the ops, then
    // structurally audit it (the in-process guard for free-count drift). The
    // authoritative metadata_csum oracle is the external e2fsck — see header.
    {
        let fs = Filesystem::mount(Arc::new(FileDevice::open(path).expect("ro"))).expect("remount");
        let report = fs_ext4::fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit");
        assert!(
            report.is_clean(),
            "[{tag}] structural anomalies after ops: {:?}",
            report.anomalies
        );
    }
    fs_ext4_test_support::assert_e2fsck_clean(path, tag);
    if std::env::var_os("RFE_KEEP_IMAGES").is_some() {
        eprintln!("[{tag}] image: {path}");
    } else {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn mk1k_file_write_truncate() {
    let p = mkfs_1k("write_trunc");
    {
        let fs = rw(&p);
        let ino = fs.apply_create("/f", 0o644).expect("create");
        fs.apply_pwrite("/f", 0, b"csum, no journal, 1k blocks\n")
            .expect("pwrite");
        fs.apply_truncate_grow(ino, 8192).expect("grow");
        fs.apply_truncate_shrink(ino, 16).expect("shrink");
    }
    done(&p, "write_trunc");
}

#[test]
fn mk1k_multiblock_write() {
    let p = mkfs_1k("multiblock");
    {
        let fs = rw(&p);
        fs.apply_create("/big", 0o644).expect("create");
        fs.apply_pwrite("/big", 0, &vec![0xABu8; 128 * 1024])
            .expect("pwrite 128K");
    }
    done(&p, "multiblock");
}

#[test]
fn mk1k_mkdir_rmdir() {
    let p = mkfs_1k("mkdir_rmdir");
    {
        let fs = rw(&p);
        fs.apply_mkdir("/d1", 0o755).expect("mkdir d1");
        fs.apply_mkdir("/d1/d2", 0o755).expect("mkdir d2");
        fs.apply_rmdir("/d1/d2").expect("rmdir d2");
        fs.apply_rmdir("/d1").expect("rmdir d1");
    }
    done(&p, "mkdir_rmdir");
}

#[test]
fn mk1k_hardlink_unlink() {
    let p = mkfs_1k("hardlink");
    {
        let fs = rw(&p);
        fs.apply_create("/a", 0o644).expect("create a");
        fs.apply_link("/a", "/b").expect("link");
        fs.apply_unlink("/a").expect("unlink a");
    }
    done(&p, "hardlink");
}

#[test]
fn mk1k_rename() {
    let p = mkfs_1k("rename");
    {
        let fs = rw(&p);
        fs.apply_create("/x", 0o644).expect("create x");
        fs.apply_create("/y", 0o644).expect("create y");
        fs.apply_rename("/x", "/z", false).expect("rename x->z");
        fs.apply_rename("/z", "/y", true)
            .expect("rename z->y overwrite");
    }
    done(&p, "rename");
}

#[test]
fn mk1k_chmod_chown() {
    let p = mkfs_1k("chmod_chown");
    {
        let fs = rw(&p);
        fs.apply_create("/m", 0o644).expect("create");
        fs.apply_chmod("/m", 0o600).expect("chmod");
        fs.apply_chown("/m", 1000, 1000).expect("chown");
    }
    done(&p, "chmod_chown");
}

/// An external xattr block next to an inline xattr, then the inline one
/// removed. This was ignored as a 1 KiB defect with "i_file_acl ... should be
/// zero". The block was sound: mkfs sets no COMPAT_EXT_ATTR, and e2fsck
/// ignores every xattr block on a volume without it. setxattr now sets the
/// feature, as the kernel does (#88).
#[test]
fn mk1k_xattr_inline_external_remove() {
    let p = mkfs_1k("xattr");
    {
        let fs = rw(&p);
        fs.apply_create("/x", 0o644).expect("create");
        fs.apply_setxattr("/x", "user.small", b"v")
            .expect("setxattr small");
        // 512 B overflows the in-inode xattr area (→ external block) but still
        // fits a single 1 KiB external block.
        fs.apply_setxattr("/x", "user.big", &vec![0x5Au8; 512])
            .expect("setxattr big (external block)");
        fs.apply_removexattr("/x", "user.small")
            .expect("removexattr");
    }
    done(&p, "xattr");
}

#[test]
fn mk1k_removexattr_last_frees_block() {
    let p = mkfs_1k("xattr_free");
    {
        let fs = rw(&p);
        fs.apply_create("/xr", 0o644).expect("create");
        // 512 B → external block that fits one 1 KiB block; removing the only
        // entry must free it.
        fs.apply_setxattr("/xr", "user.big", &vec![0x7Eu8; 512])
            .expect("setxattr big");
        fs.apply_removexattr("/xr", "user.big")
            .expect("removexattr last");
    }
    done(&p, "xattr_free");
}

#[test]
fn mk1k_fallocate_variants() {
    let p = mkfs_1k("fallocate");
    {
        let fs = rw(&p);
        let ino = fs.apply_create("/fa", 0o644).expect("create");
        fs.apply_fallocate_keep_size(ino, 0, 16384)
            .expect("keep_size");
        fs.apply_fallocate_zero_range(ino, 4096, 4096)
            .expect("zero_range");
        fs.apply_fallocate_punch_hole(ino, 8192, 4096)
            .expect("punch_hole");
    }
    done(&p, "fallocate");
}

#[test]
fn mk1k_slow_symlink_unlink() {
    let p = mkfs_1k("slow_symlink");
    {
        let fs = rw(&p);
        let target = "/a/very/long/symlink/target/path/that/exceeds/sixty/bytes/for/sure/x";
        fs.apply_symlink(target, "/sl").expect("slow symlink");
        fs.apply_unlink("/sl").expect("unlink slow symlink");
    }
    done(&p, "slow_symlink");
}

#[test]
fn mk1k_htree_dir_growth() {
    let p = mkfs_1k("htree");
    {
        let fs = rw(&p);
        fs.apply_mkdir("/h", 0o755).expect("mkdir h");
        // 1 KiB dir blocks hold few entries, so htree conversion happens with
        // fewer files; 150 stays within the small image's inode budget.
        for i in 0..150 {
            fs.apply_create(&format!("/h/file{i:04}"), 0o644)
                .unwrap_or_else(|e| panic!("create /h/file{i:04}: {e:?}"));
        }
    }
    done(&p, "htree");
}

#[test]
fn mk1k_large_chunked_write() {
    let p = mkfs_1k("large_chunked");
    {
        let fs = rw(&p);
        fs.apply_create("/lc", 0o644).expect("create");
        // 1 MiB at 1 KiB blocks far exceeds one transaction's descriptor
        // capacity, so the chunking path runs (many chunks).
        let n = fs
            .apply_pwrite("/lc", 0, &vec![0xC3u8; 1024 * 1024])
            .expect("pwrite 1MiB");
        assert_eq!(n, 1024 * 1024, "short write");
    }
    done(&p, "large_chunked");
}

#[test]
fn mk1k_fragmented_extent_tree() {
    let p = mkfs_1k("frag_extents");
    {
        let fs = rw(&p);
        fs.apply_create("/frag", 0o644).expect("create");
        for i in 0..16u64 {
            fs.apply_pwrite("/frag", i * 8192, b"x")
                .unwrap_or_else(|e| panic!("pwrite gap {i}: {e:?}"));
        }
    }
    done(&p, "frag_extents");
}
