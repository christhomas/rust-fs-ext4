//! Verify that mounting an ext4 image with a corrupted superblock checksum
//! is rejected when METADATA_CSUM is enabled.
//!
//! Strategy: copy ext4-basic.img into a tempfile, flip one byte inside the
//! superblock region (at offset 1024 + 0x100 — the first byte of the
//! `s_volume_name` field, which is covered by the checksum but not used by
//! the magic check), and confirm `Filesystem::mount` returns
//! `Error::BadChecksum`.

use ext4rs::block_io::FileDevice;
use ext4rs::error::Error;
use ext4rs::fs::Filesystem;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

const SRC_IMAGE: &str = "test-disks/ext4-basic.img";

fn copy_image_to_temp(tag: &str) -> Option<std::path::PathBuf> {
    let mut src = match std::fs::File::open(SRC_IMAGE) {
        Ok(f) => f,
        Err(_) => return None,
    };
    let tmp_path = std::env::temp_dir().join(format!(
        "ext4rs-corrupt-{}-{}.img",
        std::process::id(),
        tag
    ));
    let mut dst = std::fs::File::create(&tmp_path).expect("create temp image");
    let mut buf = Vec::new();
    src.read_to_end(&mut buf).expect("read src");
    dst.write_all(&buf).expect("write dst");
    Some(tmp_path)
}

#[test]
fn pristine_image_mounts_cleanly() {
    let Some(tmp) = copy_image_to_temp("pristine") else {
        eprintln!("skip: {SRC_IMAGE} not present");
        return;
    };
    let dev = Arc::new(FileDevice::open(tmp.to_str().unwrap()).expect("open temp image"));
    let fs = Filesystem::mount(dev).expect("mount pristine copy");
    if !fs.csum.enabled {
        eprintln!("skip: METADATA_CSUM not enabled in test image");
    }
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn corrupted_superblock_is_rejected() {
    let Some(tmp) = copy_image_to_temp("corrupt") else {
        eprintln!("skip: {SRC_IMAGE} not present");
        return;
    };

    // Probe whether checksum is enabled before corrupting.
    {
        let dev = Arc::new(FileDevice::open(tmp.to_str().unwrap()).expect("probe open"));
        let fs = Filesystem::mount(dev).expect("probe mount");
        if !fs.csum.enabled {
            eprintln!("skip: METADATA_CSUM not enabled in {SRC_IMAGE}");
            let _ = std::fs::remove_file(tmp);
            return;
        }
    }

    // Flip one bit inside the superblock-checksum-covered region.
    // Offset 1024 (start of superblock) + 0x100 = byte 0x500 in image.
    // 0x100 lands inside s_volume_name (covered by checksum, not by magic).
    {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&tmp)
            .expect("reopen rw");
        f.seek(SeekFrom::Start(1024 + 0x100)).unwrap();
        let mut byte = [0u8; 1];
        f.read_exact(&mut byte).unwrap();
        byte[0] ^= 0xFF;
        f.seek(SeekFrom::Start(1024 + 0x100)).unwrap();
        f.write_all(&byte).unwrap();
    }

    let dev = Arc::new(FileDevice::open(tmp.to_str().unwrap()).expect("open corrupted"));
    match Filesystem::mount(dev) {
        Err(Error::BadChecksum { what }) => {
            assert_eq!(what, "superblock");
        }
        Ok(_) => panic!("mount accepted corrupted superblock"),
        Err(e) => panic!("mount returned wrong error: {e:?}"),
    }

    let _ = std::fs::remove_file(tmp);
}
