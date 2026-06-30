//! Wide op-coverage sweep on a NON-metadata_csum, NON-journaled filesystem
//! (ext4-no-csum.img). The sister sweep in `repro_csum_seed_op_coverage.rs`
//! exercises the journaled + checksummed write path; this one exercises the
//! OTHER path: every checksum recompute is `if csum.enabled`-gated (skipped
//! here), and with no `has_journal` feature the apply_* ops write directly
//! rather than through a JournalWriter transaction. `all_images_rw_smoke`
//! already does a trivial create/write/unlink here, but with no e2fsck oracle;
//! this adds the full op matrix and leaves each mutated image in /tmp for a
//! real Linux e2fsck pass:
//!
//!   scripts/vm-e2fsck.sh /tmp/fs_ext4_nocsum_*.img
//!
//! ext4-no-csum.img is small (~3.4 MiB free, 4 KiB blocks) but each test gets
//! its own fresh copy, so the ops are sized to fit one image at a time.

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn copy(tag: &str) -> Option<String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let src = format!("{}/test-disks/ext4-no-csum.img", env!("CARGO_MANIFEST_DIR"));
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!("/tmp/fs_ext4_nocsum_{tag}_{}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn rw(path: &str) -> Filesystem {
    Filesystem::mount(Arc::new(FileDevice::open_rw(path).expect("open_rw"))).expect("mount")
}

fn done(path: &str, tag: &str) {
    // Re-mount read-only to confirm the image still mounts after the ops.
    // (There is no journal to check; the authoritative oracle is an external
    // e2fsck — see the file header.)
    {
        let _ = Filesystem::mount(Arc::new(FileDevice::open(path).expect("ro"))).expect("remount");
    }
    if std::env::var_os("RFE_KEEP_IMAGES").is_some() {
        eprintln!("[{tag}] image: {path}");
    } else {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn nocsum_file_write_truncate() {
    let Some(p) = copy("write_trunc") else { return };
    {
        let fs = rw(&p);
        let ino = fs.apply_create("/f", 0o644).expect("create");
        fs.apply_pwrite("/f", 0, b"no checksum, no journal\n")
            .expect("pwrite");
        fs.apply_truncate_grow(ino, 8192).expect("grow");
        fs.apply_truncate_shrink(ino, 16).expect("shrink");
    }
    done(&p, "write_trunc");
}

#[test]
fn nocsum_multiblock_write() {
    let Some(p) = copy("multiblock") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/big", 0o644).expect("create");
        fs.apply_pwrite("/big", 0, &vec![0xABu8; 256 * 1024])
            .expect("pwrite 256K");
    }
    done(&p, "multiblock");
}

#[test]
fn nocsum_mkdir_rmdir() {
    let Some(p) = copy("mkdir_rmdir") else { return };
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
fn nocsum_hardlink_unlink() {
    let Some(p) = copy("hardlink") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/a", 0o644).expect("create a");
        fs.apply_link("/a", "/b").expect("link");
        fs.apply_unlink("/a").expect("unlink a");
    }
    done(&p, "hardlink");
}

#[test]
fn nocsum_rename() {
    let Some(p) = copy("rename") else { return };
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
fn nocsum_chmod_chown() {
    let Some(p) = copy("chmod_chown") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/m", 0o644).expect("create");
        fs.apply_chmod("/m", 0o600).expect("chmod");
        fs.apply_chown("/m", 1000, 1000).expect("chown");
    }
    done(&p, "chmod_chown");
}

#[test]
fn nocsum_xattr_inline_external_remove() {
    let Some(p) = copy("xattr") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/x", 0o644).expect("create");
        fs.apply_setxattr("/x", "user.small", b"v")
            .expect("setxattr small");
        fs.apply_setxattr("/x", "user.big", &vec![0x5Au8; 2048])
            .expect("setxattr big (external block)");
        fs.apply_removexattr("/x", "user.small")
            .expect("removexattr");
    }
    done(&p, "xattr");
}

#[test]
fn nocsum_removexattr_last_frees_block() {
    let Some(p) = copy("xattr_free") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/xr", 0o644).expect("create");
        fs.apply_setxattr("/xr", "user.big", &vec![0x7Eu8; 3072])
            .expect("setxattr big");
        fs.apply_removexattr("/xr", "user.big")
            .expect("removexattr last");
    }
    done(&p, "xattr_free");
}

#[test]
fn nocsum_fallocate_variants() {
    let Some(p) = copy("fallocate") else { return };
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
fn nocsum_slow_symlink_unlink() {
    let Some(p) = copy("slow_symlink") else {
        return;
    };
    {
        let fs = rw(&p);
        let target = "/a/very/long/symlink/target/path/that/exceeds/sixty/bytes/for/sure/x";
        fs.apply_symlink(target, "/sl").expect("slow symlink");
        fs.apply_unlink("/sl").expect("unlink slow symlink");
    }
    done(&p, "slow_symlink");
}

#[test]
fn nocsum_htree_dir_growth() {
    let Some(p) = copy("htree") else { return };
    {
        let fs = rw(&p);
        fs.apply_mkdir("/h", 0o755).expect("mkdir h");
        // Enough entries to overflow a single dir block and convert to htree,
        // kept well below the ~500-entry htree-write edge case and within the
        // small image's inode budget.
        for i in 0..150 {
            fs.apply_create(&format!("/h/file{i:04}"), 0o644)
                .unwrap_or_else(|e| panic!("create /h/file{i:04}: {e:?}"));
        }
    }
    done(&p, "htree");
}

#[test]
fn nocsum_large_chunked_write() {
    let Some(p) = copy("large_chunked") else {
        return;
    };
    {
        let fs = rw(&p);
        fs.apply_create("/lc", 0o644).expect("create");
        // > 1 transaction's descriptor capacity at 4 KiB blocks, so the
        // chunking path runs even though there is no journal here.
        let n = fs
            .apply_pwrite("/lc", 0, &vec![0xC3u8; 2 * 1024 * 1024])
            .expect("pwrite 2MiB");
        assert_eq!(n, 2 * 1024 * 1024, "short write");
    }
    done(&p, "large_chunked");
}

#[test]
fn nocsum_fragmented_extent_tree() {
    let Some(p) = copy("frag_extents") else {
        return;
    };
    {
        let fs = rw(&p);
        fs.apply_create("/frag", 0o644).expect("create");
        // Logically-gapped single-block writes → many separate extents that
        // overflow the 4 inline slots into an external extent block (depth >= 1).
        for i in 0..16u64 {
            fs.apply_pwrite("/frag", i * 8192, b"x")
                .unwrap_or_else(|e| panic!("pwrite gap {i}: {e:?}"));
        }
    }
    done(&p, "frag_extents");
}
