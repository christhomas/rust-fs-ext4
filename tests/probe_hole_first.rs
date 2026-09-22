//! Scratch: read a hole below the first index entry of a deep tree.
use fs_ext4::block_io::FileDevice;
use fs_ext4::{file_io, fs::Filesystem};
use std::process::Command;
use std::sync::Arc;

#[test]
fn hole_before_the_first_index_entry() {
    let image = format!(
        "{}/probe-hole-{}.img",
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
    let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap())).unwrap();
    let ino = fs.apply_create("/sparse", 0o644).unwrap();
    // Nothing at block 0: the first extent is at logical block 16.
    for i in 1..800u64 {
        fs.apply_pwrite("/sparse", i * 16 * 4096, &[7u8; 4096])
            .unwrap();
    }
    let (inode, _) = fs.read_inode_verified(ino).unwrap();
    let depth = u16::from_le_bytes(inode.block[6..8].try_into().unwrap());
    let mut buf = vec![0u8; 4096];
    let r = file_io::read(&fs, &inode, 0, 4096, &mut buf);
    eprintln!("depth {depth}, reading logical block 0: {r:?}");
    let _ = std::fs::remove_file(&image);
    assert!(
        r.is_ok(),
        "a hole below the first index entry must read as zeros"
    );
}
