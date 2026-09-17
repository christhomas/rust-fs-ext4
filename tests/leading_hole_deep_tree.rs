//! A hole below the first entry of a deep extent tree reads as zeros (#260).
//!
//! `descend` kept the last index entry whose `ei_block` is at or below the
//! block being mapped, and refused the map when there was none — which is
//! every block before the first one a sparse file holds. So an ordinary file
//! whose data starts past block 0 could not be read at its leading hole, and
//! the error said the extent tree was corrupt on a volume `e2fsck` accepts.
//!
//! The kernel's `ext4_ext_binsearch_idx` leaves the path at the first index
//! entry in that case, descends, finds no extent covering the block, and
//! reports a hole.
//!
//! Here a file is written one block every 16 blocks from logical block 16
//! upward, so the tree is deeper than the inode and nothing covers block 0.
//! e2fsprogs is required.

use fs_ext4::block_io::FileDevice;
use fs_ext4::file_io;
use fs_ext4::fs::Filesystem;
use std::process::Command;
use std::sync::Arc;

fn tool(name: &str) -> String {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|p| std::path::Path::new(p).exists())
        .unwrap_or_else(|| panic!("{name} is not installed; install e2fsprogs"))
}

#[test]
fn a_hole_below_the_first_index_entry_reads_as_zeros() {
    let image = fs_ext4_test_support::temp_path!("fs_ext4_260_{}.img", std::process::id());
    std::fs::File::create(&image)
        .and_then(|f| f.set_len(128 * 1024 * 1024))
        .unwrap();
    let mkfs = Command::new(tool("mkfs.ext4"))
        .args(["-q", "-F", "-b", "4096"])
        .arg(&image)
        .output()
        .unwrap();
    assert!(
        mkfs.status.success(),
        "{}",
        String::from_utf8_lossy(&mkfs.stderr)
    );

    let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap())).unwrap();
    let ino = fs.apply_create("/sparse", 0o644).expect("create");
    // Nothing at block 0: the first extent is at logical block 16, and one
    // block every 16 keeps each of them a record of its own.
    for i in 1..800u64 {
        fs.apply_pwrite("/sparse", i * 16 * 4096, &[7u8; 4096])
            .unwrap_or_else(|e| panic!("write block {}: {e:?}", i * 16));
    }
    let (inode, _) = fs.read_inode_verified(ino).unwrap();
    let depth = u16::from_le_bytes(inode.block[6..8].try_into().unwrap());
    assert!(
        depth >= 1,
        "the tree is still inline, so nothing here descends an index"
    );

    let mut buf = vec![0xFFu8; 4096];
    for hole in [0u64, 4096, 15 * 4096] {
        let n = file_io::read(&fs, &inode, hole, 4096, &mut buf)
            .unwrap_or_else(|e| panic!("reading the hole at {hole}: {e:?}"));
        assert_eq!(n, 4096, "short read at {hole}");
        assert!(
            buf.iter().all(|&b| b == 0),
            "the hole at {hole} read back {:?}",
            &buf[..8]
        );
    }

    // The data past it still reads, which is what says the descent did not
    // simply stop looking.
    let n = file_io::read(&fs, &inode, 16 * 4096, 4096, &mut buf).expect("read the first block");
    assert_eq!(n, 4096);
    assert!(buf.iter().all(|&b| b == 7), "{:?}", &buf[..8]);

    drop(fs);
    let out = Command::new(tool("e2fsck"))
        .args(["-fn", &image])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "e2fsck rejected the volume:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_file(&image);
}
