//! Wide op-coverage sweep on a metadata_csum filesystem (ext4-csum-seed.img),
//! to flush out remaining write-path checksum/metadata bugs beyond the original
//! mkdir+symlink+unlink repro. Each test exercises one op class against a fresh
//! copy and leaves the mutated image in /tmp (path printed) for an Alpine-VM
//! e2fsck pass:
//!
//!   scripts/vm-e2fsck.sh /tmp/fs_ext4_wild_*.img
//!
//! The driver's own readers can't see metadata_csum mistakes, so a real Linux
//! e2fsck is the oracle. `is_clean()` here is just a cheap smoke check.

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn copy(tag: &str) -> Option<String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let src = format!(
        "{}/test-disks/ext4-csum-seed.img",
        env!("CARGO_MANIFEST_DIR")
    );
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!("/tmp/fs_ext4_wild_{tag}_{}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn rw(path: &str) -> Filesystem {
    Filesystem::mount(Arc::new(FileDevice::open_rw(path).expect("open_rw"))).expect("mount")
}

fn done(path: &str, tag: &str) {
    {
        let fs = Filesystem::mount(Arc::new(FileDevice::open(path).expect("ro"))).expect("remount");
        if let Some(j) = fs_ext4::jbd2::read_superblock(&fs).expect("jsb") {
            assert!(j.is_clean(), "[{tag}] journal not clean after ops");
        }
    }
    // The authoritative check is an external e2fsck (see the file header) — the
    // in-process readers can't see metadata_csum mistakes. Keep the image only
    // when explicitly hunting (RFE_KEEP_IMAGES set); otherwise clean up so the
    // committed test leaves nothing in /tmp.
    if std::env::var_os("RFE_KEEP_IMAGES").is_some() {
        eprintln!("[{tag}] image: {path}");
    } else {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn op_file_write_truncate() {
    let Some(p) = copy("write_trunc") else { return };
    {
        let fs = rw(&p);
        let ino = fs.apply_create("/f", 0o644).expect("create");
        fs.apply_pwrite("/f", 0, b"hello metadata_csum extents\n")
            .expect("pwrite");
        fs.apply_truncate_grow(ino, 8192).expect("grow");
        fs.apply_truncate_shrink(ino, 16).expect("shrink");
    }
    done(&p, "write_trunc");
}

#[test]
fn op_bigwrite_multiblock() {
    let Some(p) = copy("bigwrite") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/big", 0o644).expect("create");
        let data = vec![0xABu8; 64 * 1024];
        fs.apply_pwrite("/big", 0, &data).expect("pwrite");
    }
    done(&p, "bigwrite");
}

#[test]
fn op_mkdir_rmdir() {
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
fn op_hardlink_unlink() {
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
fn op_rename() {
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
fn op_chmod_chown() {
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
fn op_xattr_inline_and_external() {
    let Some(p) = copy("xattr") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/x", 0o644).expect("create");
        fs.apply_setxattr("/x", "user.small", b"v")
            .expect("setxattr small");
        let big = vec![0x5Au8; 2048]; // overflows in-inode → external xattr block
        fs.apply_setxattr("/x", "user.big", &big)
            .expect("setxattr big");
        fs.apply_removexattr("/x", "user.small")
            .expect("removexattr");
    }
    done(&p, "xattr");
}

#[test]
fn op_fallocate_variants() {
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

// ── round 2: trickier structural paths ──────────────────────────────────────

#[test]
fn op_rename_dir_crossdir() {
    let Some(p) = copy("rename_dir") else { return };
    {
        let fs = rw(&p);
        fs.apply_mkdir("/a", 0o755).expect("mkdir a");
        fs.apply_mkdir("/b", 0o755).expect("mkdir b");
        fs.apply_mkdir("/a/m", 0o755).expect("mkdir a/m");
        // Move a directory across parents: m's ".." must switch a -> b, and the
        // two parents' link counts must follow.
        fs.apply_rename("/a/m", "/b/m", false)
            .expect("rename dir a/m -> b/m");
    }
    done(&p, "rename_dir");
}

#[test]
fn op_htree_dir_growth() {
    let Some(p) = copy("htree") else { return };
    {
        let fs = rw(&p);
        fs.apply_mkdir("/h", 0o755).expect("mkdir h");
        // Enough entries to overflow a single dir block and convert to htree.
        for i in 0..240 {
            fs.apply_create(&format!("/h/file{i:04}"), 0o644)
                .unwrap_or_else(|e| panic!("create /h/file{i:04}: {e:?}"));
        }
    }
    done(&p, "htree");
}

#[test]
fn op_dir_multiblock_growth() {
    let Some(p) = copy("dir_multiblock") else {
        return;
    };
    {
        let fs = rw(&p);
        fs.apply_mkdir("/m", 0o755).expect("mkdir m");
        for i in 0..60 {
            fs.apply_create(&format!("/m/f{i:03}"), 0o644)
                .unwrap_or_else(|e| panic!("create /m/f{i:03}: {e:?}"));
        }
    }
    done(&p, "dir_multiblock");
}

#[test]
fn op_slow_symlink() {
    let Some(p) = copy("slow_symlink") else {
        return;
    };
    {
        let fs = rw(&p);
        // > 60 bytes → "slow" symlink: target stored in a data block, not i_block.
        let target = "/a/very/long/symlink/target/path/that/exceeds/sixty/bytes/for/sure/x";
        fs.apply_symlink(target, "/slink").expect("slow symlink");
    }
    done(&p, "slow_symlink");
}

#[test]
fn op_large_file_extents() {
    let Some(p) = copy("large_file") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/lf", 0o644).expect("create");
        // 2 MiB — larger than one journal transaction's descriptor capacity
        // (~255 blocks), so apply_pwrite must split it into block-aligned
        // chunks that each commit as their own transaction. Pre-fix this
        // failed with "descriptor block overflow (too many tags)".
        let data = vec![0xC3u8; 2 * 1024 * 1024];
        let n = fs.apply_pwrite("/lf", 0, &data).expect("pwrite 2MiB");
        assert_eq!(n, data.len() as u64, "short write");
    }
    done(&p, "large_file");
}

// ── round 3: free-path + replace + slow-symlink teardown ─────────────────────

#[test]
fn op_replace_file_content() {
    let Some(p) = copy("replace") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/rf", 0o644).expect("create");
        fs.apply_pwrite("/rf", 0, &vec![0x11u8; 20480])
            .expect("pwrite");
        // Full rewrite to a different size frees the old extents + allocs new.
        fs.apply_replace_file_content("/rf", &vec![0x22u8; 4096])
            .expect("replace");
    }
    done(&p, "replace");
}

#[test]
fn op_removexattr_last_frees_block() {
    let Some(p) = copy("xattr_free") else { return };
    {
        let fs = rw(&p);
        fs.apply_create("/xr", 0o644).expect("create");
        // Large value → external xattr block; removing the only entry should
        // free the block and clear i_file_acl (+ recompute the inode checksum).
        fs.apply_setxattr("/xr", "user.big", &vec![0x7Eu8; 3072])
            .expect("setxattr big");
        fs.apply_removexattr("/xr", "user.big")
            .expect("removexattr last");
    }
    done(&p, "xattr_free");
}

#[test]
fn op_slow_symlink_unlink() {
    let Some(p) = copy("slow_symlink_unlink") else {
        return;
    };
    {
        let fs = rw(&p);
        let target = "/a/very/long/symlink/target/path/that/exceeds/sixty/bytes/for/sure/x";
        fs.apply_symlink(target, "/sl").expect("slow symlink");
        // Unlink the slow symlink → frees its inode AND its data block.
        fs.apply_unlink("/sl").expect("unlink slow symlink");
    }
    done(&p, "slow_symlink_unlink");
}

#[test]
fn op_fragmented_extent_tree() {
    let Some(p) = copy("frag_extents") else {
        return;
    };
    {
        let fs = rw(&p);
        fs.apply_create("/frag", 0o644).expect("create");
        // Logically-gapped single-block writes → many separate extents. >4
        // extents overflow the 4 inline slots in the inode's i_block and spill
        // to an external extent block (tree depth >= 1), whose ext4_extent_tail
        // checksum must be computed on write. All our other files are
        // single-extent, so this is the only test that exercises that path.
        for i in 0..16u64 {
            fs.apply_pwrite("/frag", i * 8192, b"x")
                .unwrap_or_else(|e| panic!("pwrite gap {i}: {e:?}"));
        }
    }
    done(&p, "frag_extents");
}
