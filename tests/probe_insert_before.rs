//! Scratch: insert an extent before the first entry of a deep tree's leaf.
use fs_ext4::block_io::FileDevice;
use fs_ext4::fs::Filesystem;
use std::process::Command;
use std::sync::Arc;

#[test]
fn insert_before_first_entry() {
    let image = format!(
        "{}/probe-ib-{}.img",
        std::env::temp_dir().display(),
        std::process::id()
    );
    std::fs::File::create(&image)
        .and_then(|f| f.set_len(128 * 1024 * 1024))
        .unwrap();
    assert!(Command::new("/sbin/mkfs.ext4")
        .args(["-q", "-F", "-b", "4096"])
        .arg(&image)
        .status()
        .unwrap()
        .success());
    {
        let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap())).unwrap();
        fs.apply_create("/sparse", 0o644).unwrap();
        // Data from logical block 16 upward, so the tree is deep and nothing
        // is at block 0.
        for i in 1..800u64 {
            fs.apply_pwrite("/sparse", i * 16 * 4096, &[7u8; 4096])
                .unwrap();
        }
    }
    let out = Command::new("/sbin/e2fsck")
        .args(["-fn", &image])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "before the leading write:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    {
        let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap())).unwrap();
        fs.apply_pwrite("/sparse", 0, &[9u8; 4096])
            .expect("write block 0");
    }
    let out = Command::new("/sbin/e2fsck")
        .args(["-fn", &image])
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_file(&image);
    assert!(
        out.status.success(),
        "after writing logical block 0:\n{report}"
    );
}
