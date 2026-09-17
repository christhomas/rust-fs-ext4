//! A 1 KiB-block volume one block past a multiple of the group size
//! mounts, with the group count `mke2fs` gave it (#86).
//!
//! The group count divided the whole `s_blocks_count` instead of starting
//! at `s_first_data_block`, so a `mkfs.ext4 -b 1024` image of 16385 blocks
//! read as three groups where it has two; the phantom descriptor's zeros
//! failed their checksum and the mount was refused. Fails without
//! e2fsprogs (`chore tools`).

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::fs::Filesystem;

use std::process::Command;
use std::sync::Arc;

#[test]
fn a_one_kib_volume_one_block_past_a_group_boundary_mounts_with_its_real_group_count() {
    let dir =
        fs_ext4_test_support::temp_dir().join(format!("ext4-group-count-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("g.img");
    std::fs::File::create(&img)
        .unwrap()
        .set_len(16385 * 1024)
        .unwrap();
    let made = Command::new(fs_ext4_test_support::oracle_tool("mkfs.ext4"))
        .args(["-q", "-F", "-b", "1024", "-O", "metadata_csum"])
        .arg(&img)
        .arg("16385")
        .output()
        .expect("run mkfs.ext4");
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );

    let dev = FileDevice::open(img.to_str().unwrap()).unwrap();
    let fs = Filesystem::mount(Arc::new(dev) as Arc<dyn BlockDevice>)
        .unwrap_or_else(|e| panic!("a volume mke2fs made must mount: {e:?}"));
    assert_eq!(fs.sb.blocks_count, 16385);
    assert_eq!(fs.sb.first_data_block, 1);
    assert_eq!(
        fs.sb.block_group_count(),
        2,
        "dumpe2fs lists groups 0 and 1"
    );
    assert_eq!(fs.groups.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}
