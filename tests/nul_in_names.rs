//! A name holding a NUL byte is refused by every operation that files one.
//!
//! A directory entry's name is counted bytes, not a C string, so nothing in
//! the on-disk format stops a NUL. The kernel never writes one, because a
//! name reaches it as a C string, and `e2fsck` reports one as an illegal
//! character in the name. This crate takes a Rust `&str`, which can hold
//! NUL, and filed it as given: create, mkdir, mknod, symlink, link and
//! rename all wrote the entry.
//!
//! Volumes come from `mkfs.ext4`, and `e2fsck -fn` must accept each one after
//! the refused operation. Skips without e2fsprogs.

use fs_ext4::block_io::FileDevice;
use fs_ext4::fs::Filesystem;
use std::process::Command;
use std::sync::Arc;

fn mkfs(tag: &str) -> Option<String> {
    let mkfs = ["/usr/sbin/mkfs.ext4", "/sbin/mkfs.ext4"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())?;
    let path = fs_ext4_test_support::temp_path!("fs_ext4_nul_{tag}_{}.img", std::process::id());
    std::fs::File::create(&path)
        .and_then(|f| f.set_len(64 * 1024 * 1024))
        .unwrap();
    let out = Command::new(mkfs)
        .args(["-q", "-F", "-b", "4096"])
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
fn a_name_holding_a_nul_byte_is_refused() {
    type Op = fn(&Filesystem) -> Result<(), fs_ext4::Error>;
    let ops: [(&str, Op); 6] = [
        ("create", |fs| fs.apply_create("/a\0b", 0o644).map(|_| ())),
        ("mkdir", |fs| fs.apply_mkdir("/a\0b", 0o755).map(|_| ())),
        ("mknod", |fs| {
            fs.apply_mknod("/a\0b", 0o010644, 0, 0).map(|_| ())
        }),
        ("symlink", |fs| {
            fs.apply_symlink("target", "/a\0b").map(|_| ())
        }),
        ("link", |fs| fs.apply_link("/f", "/a\0b")),
        ("rename", |fs| fs.apply_rename("/f", "/a\0b", false)),
    ];
    for (tag, op) in ops {
        let Some(path) = mkfs(tag) else {
            eprintln!("skip: e2fsprogs not installed");
            return;
        };
        let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(&path).unwrap())).unwrap();
        fs.apply_create("/f", 0o644).unwrap();
        let got = op(&fs);
        drop(fs);
        assert!(got.is_err(), "[{tag}] filed a name holding a NUL byte");
        let out = Command::new("e2fsck")
            .args(["-fn", &path])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "[{tag}] e2fsck -fn rejected the volume:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let _ = std::fs::remove_file(&path);
    }
}
