//! Replacing a file's content keeps its xattr block in `i_blocks` (#251).
//!
//! `i_blocks` counts every block an inode holds, the external xattr block
//! included. `apply_replace_file_content` set it to exactly the new data
//! (and indirect) blocks, so a file with an external xattr block came out one
//! block short, and e2fsck reported `i_blocks is 8, should be 16`. Found by a
//! seeded random sequence of operations checked with e2fsck.
//!
//! Volumes come from `mkfs.ext4`, extent-mapped and block-mapped, and
//! `e2fsck -fn` judges each. Skips without e2fsprogs.

use fs_ext4::block_io::FileDevice;
use fs_ext4::fs::Filesystem;
use std::process::Command;
use std::sync::Arc;

fn mkfs(tag: &str, features: &str) -> Option<String> {
    let mkfs = ["/usr/sbin/mkfs.ext4", "/sbin/mkfs.ext4"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())?;
    let path =
        fs_ext4_test_support::temp_path!("fs_ext4_replace_xattr_{tag}_{}.img", std::process::id());
    std::fs::File::create(&path)
        .and_then(|f| f.set_len(64 * 1024 * 1024))
        .unwrap();
    let out = Command::new(mkfs)
        .args(["-q", "-F", "-b", "4096", "-I", "256", "-O", features])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(path)
}

#[test]
fn replacing_content_keeps_the_xattr_block_counted() {
    for (tag, features) in [
        ("extents", "extent"),
        ("blockmap", "^extent,^64bit,^metadata_csum"),
    ] {
        for (what, len) in [("data", 20_000usize), ("empty", 0)] {
            let Some(path) = mkfs(&format!("{tag}_{what}"), features) else {
                eprintln!("skip: e2fsprogs not installed");
                return;
            };
            let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(&path).unwrap())).unwrap();
            fs.apply_create("/a", 0o644).unwrap();
            fs.apply_replace_file_content("/a", &[5u8; 9000]).unwrap();
            fs.apply_setxattr("/a", "user.big", &[b'v'; 600]).unwrap();
            let (inode, _) = {
                let mut r = |i: u32| fs.read_inode_verified(i).map(|(x, _)| x);
                let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut r, "/a").unwrap();
                fs.read_inode_verified(ino).unwrap()
            };
            assert_ne!(
                inode.file_acl, 0,
                "[{tag} {what}] the attribute stayed in the inode"
            );
            fs.apply_replace_file_content("/a", &vec![6u8; len])
                .unwrap();
            drop(fs);
            let out = Command::new("e2fsck")
                .args(["-fn", &path])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "[{tag} {what}] e2fsck -fn rejected the volume:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
            let _ = std::fs::remove_file(&path);
        }
    }
}
