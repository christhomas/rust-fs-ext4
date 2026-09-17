//! A hole can be punched in a file whose extent tree is deeper than the
//! inode's inline root (#258).
//!
//! Punching rebuilt what survived into the inode's four inline entries and
//! freed every node below it. Any file with more than four surviving extents
//! was therefore refused outright — and a file with a deep tree is exactly
//! the large file a punch is for. A scale probe had 750 of 750 punches
//! refused, on 4 KiB and on 1 KiB blocks.
//!
//! Here a file is written one block every 64 KiB, so each block is its own
//! extent and the tree needs several leaves. Punching every other one must
//! leave a volume `e2fsck -fn` accepts, whose surviving blocks still read
//! back their own bytes and whose punched ones read as zeros, and the blocks
//! must go back: `i_blocks` falls by what was freed. Writing the holes again
//! must then work. e2fsprogs is required.

use fs_ext4::block_io::FileDevice;
use fs_ext4::file_io;
use fs_ext4::fs::Filesystem;
use std::process::Command;
use std::sync::Arc;

/// Blocks in the file, one every `STRIDE` bytes.
const EXTENTS: u64 = 800;

fn tool(name: &str) -> String {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|p| std::path::Path::new(p).exists())
        .unwrap_or_else(|| panic!("{name} is not installed; install e2fsprogs"))
}

fn e2fsck_clean(image: &str, what: &str) {
    let out = Command::new(tool("e2fsck"))
        .args(["-fn", image])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "[{what}] e2fsck rejected the volume:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn mount(image: &str) -> Filesystem {
    Filesystem::mount(Arc::new(FileDevice::open_rw(image).unwrap())).unwrap()
}

/// The byte every block of `i` is filled with, so a block read back says
/// which extent it came from.
fn fill(i: u64) -> u8 {
    (i % 251 + 1) as u8
}

fn punch_a_striped_file(tag: &str, block_size: u64) {
    let image = fs_ext4_test_support::temp_path!("fs_ext4_258_{tag}_{}.img", std::process::id());
    std::fs::File::create(&image)
        .and_then(|f| f.set_len(256 * 1024 * 1024))
        .unwrap();
    let mkfs = Command::new(tool("mkfs.ext4"))
        .args(["-q", "-F", "-b", &block_size.to_string()])
        .arg(&image)
        .output()
        .unwrap();
    assert!(
        mkfs.status.success(),
        "{}",
        String::from_utf8_lossy(&mkfs.stderr)
    );

    // One block every 16 blocks, so no two extents are adjacent and each is
    // a record of its own.
    let stride = block_size * 16;
    let (ino, blocks_before) = {
        let fs = mount(&image);
        let ino = fs.apply_create("/striped", 0o644).expect("create");
        for i in 0..EXTENTS {
            fs.apply_pwrite("/striped", i * stride, &vec![fill(i); block_size as usize])
                .unwrap_or_else(|e| panic!("write extent {i}: {e:?}"));
        }
        let (inode, _) = fs.read_inode_verified(ino).unwrap();
        let depth = u16::from_le_bytes(inode.block[6..8].try_into().unwrap());
        assert!(
            depth >= 1,
            "{tag}: the tree is still inline, so this tests nothing"
        );
        (ino, inode.blocks)
    };
    e2fsck_clean(&image, "after writing the stripes");

    // Punch every other one.
    {
        let fs = mount(&image);
        for i in (0..EXTENTS).step_by(2) {
            fs.apply_fallocate_punch_hole(ino, i * stride, block_size)
                .unwrap_or_else(|e| panic!("punch extent {i}: {e:?}"));
        }
    }
    e2fsck_clean(&image, "after punching every other extent");

    let fs = mount(&image);
    let (inode, _) = fs.read_inode_verified(ino).unwrap();
    let sectors = block_size / 512;
    // The data blocks go back, and so do the tree blocks the repack no
    // longer needs — but never more than the tree had. e2fsck above is what
    // says the count is exactly right; this says the punch gave blocks back
    // at all, which a punch that only rewrote the tree would not.
    let freed_data = (EXTENTS / 2) * sectors;
    assert!(
        inode.blocks <= blocks_before - freed_data,
        "{tag}: i_blocks went from {blocks_before} to {}, which is less than the \
         {freed_data} sectors of data the punch freed",
        inode.blocks
    );

    let mut buf = vec![0u8; block_size as usize];
    for i in 0..EXTENTS {
        let n = file_io::read(&fs, &inode, i * stride, block_size, &mut buf).expect("read");
        assert_eq!(n, block_size, "{tag}: short read at extent {i}");
        let want = if i % 2 == 0 { 0 } else { fill(i) };
        assert!(
            buf.iter().all(|&b| b == want),
            "{tag}: extent {i} reads back {:?}, expected {want}",
            &buf[..8]
        );
    }
    drop(fs);

    // The holes take data again.
    {
        let fs = mount(&image);
        for i in (0..EXTENTS).step_by(2) {
            fs.apply_pwrite("/striped", i * stride, &vec![0xC3; block_size as usize])
                .unwrap_or_else(|e| panic!("refill extent {i}: {e:?}"));
        }
    }
    e2fsck_clean(&image, "after writing the holes again");

    let fs = mount(&image);
    let (inode, _) = fs.read_inode_verified(ino).unwrap();
    for i in (0..EXTENTS).step_by(2) {
        file_io::read(&fs, &inode, i * stride, block_size, &mut buf).expect("read");
        assert!(
            buf.iter().all(|&b| b == 0xC3),
            "{tag}: refilled extent {i} reads back {:?}",
            &buf[..8]
        );
    }
    drop(fs);
    let _ = std::fs::remove_file(&image);
}

#[test]
fn punching_a_deep_tree_on_4k_blocks() {
    punch_a_striped_file("b4096", 4096);
}

#[test]
fn punching_a_deep_tree_on_1k_blocks() {
    punch_a_striped_file("b1024", 1024);
}
