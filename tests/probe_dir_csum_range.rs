//! Diagnostic probe: compute the directory-block checksum two ways and see
//! which matches the stored `det_checksum` on a real ext4 image.
//!
//! Context: capi_basic `stat_non_root_path` and every other stat-via-path
//! test started failing with "directory block checksum mismatch" after
//! `verify_dir_entry_tail` got wired into `find_entry_linear`. @6's
//! implementation hashes `block[..len-12]`. I believe the kernel recipe
//! is `block[..len-4]` (excludes only the 4-byte det_checksum field,
//! NOT the full 12-byte tail). This test settles it empirically.

use ext4rs::bgd;
use ext4rs::block_io::{BlockDevice, FileDevice};
use ext4rs::checksum::Checksummer;
use ext4rs::fs::Filesystem;
use ext4rs::inode::Inode;
use std::sync::Arc;

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

/// Reimplementation of the Linux `crc32c_le` used by ext4.
/// Equivalent to `crc32c::crc32c_append(!initial, data)` xor-inverted back.
fn linux_crc32c(initial: u32, data: &[u8]) -> u32 {
    // ext4bridge already exposes this; pull it back via its public API.
    // We chain by computing with the seed as the initial value.
    let mut h = initial;
    for chunk in data.chunks(1) {
        h = crc32c::crc32c_append(!h, chunk);
        h = !h;
    }
    // Mirror exactly what ext4rs does internally — cheaper one-shot:
    let mut h2 = initial;
    h2 = !h2;
    h2 = crc32c::crc32c_append(h2, data);
    !h2 // extra no-op to silence unused
}

#[test]
fn determine_which_hash_range_matches_real_block() {
    let dev = Arc::new(FileDevice::open(IMAGE).expect("open"));
    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount");

    // Read the root directory's first data block.
    let (ino_block, ino_off) = bgd::locate_inode(&fs.sb, &fs.groups, 2).expect("locate root");
    let ino_block_data = fs.read_block(ino_block).expect("read inode block");
    let inode_size = fs.sb.inode_size as usize;
    let root = Inode::parse(&ino_block_data[ino_off as usize..ino_off as usize + inode_size])
        .expect("parse root");

    // Root dir's first logical block — map via extent tree.
    let bs = fs.sb.block_size();
    let phys = ext4rs::extent::map_logical(&root.block, dev_dyn.as_ref(), bs, 0)
        .expect("map")
        .expect("root dir should have block 0");
    let mut block = vec![0u8; bs as usize];
    dev_dyn
        .read_at(phys * bs as u64, &mut block)
        .expect("read block");

    let end = block.len();
    // Sanity: this image should have metadata_csum enabled and a dir tail.
    let inode_zero = u32::from_le_bytes(block[end - 12..end - 8].try_into().unwrap());
    let rec_len = u16::from_le_bytes(block[end - 8..end - 6].try_into().unwrap());
    let ft_marker = block[end - 5];
    eprintln!(
        "tail probe: inode={inode_zero} rec_len={rec_len} ft=0x{ft_marker:02x} (want 0, 12, 0xDE)"
    );
    assert_eq!(inode_zero, 0, "block lacks dir tail; test image wrong");
    assert_eq!(rec_len, 12, "dir tail rec_len");
    assert_eq!(ft_marker, 0xDE, "dir tail file type marker");

    let stored = u32::from_le_bytes(block[end - 4..end].try_into().unwrap());

    let csummer = Checksummer::from_superblock(&fs.sb);
    eprintln!("seed=0x{:08x} enabled={}", csummer.seed, csummer.enabled);

    // Two candidate hash ranges.
    let hash = |range_end: usize| -> u32 {
        let mut c = csummer.seed;
        let mut go = |data: &[u8]| {
            c = !c;
            c = crc32c::crc32c_append(c, data);
            c = !c;
        };
        go(&2u32.to_le_bytes());
        go(&root.generation.to_le_bytes());
        go(&block[..range_end]);
        c
    };

    let hash_minus12 = hash(end - 12);
    let hash_minus4 = hash(end - 4);

    eprintln!("stored     = 0x{stored:08x}");
    eprintln!("hash..end-12 = 0x{hash_minus12:08x}");
    eprintln!("hash..end-4  = 0x{hash_minus4:08x}");

    // Exactly one should match. The assertion below tells us which recipe
    // the kernel used when ext4 formatter built this image.
    let m12 = hash_minus12 == stored;
    let m4 = hash_minus4 == stored;
    assert!(
        m12 ^ m4 || !(m12 || m4),
        "both ranges matched? impossible — bug in probe"
    );
    if m4 {
        eprintln!("VERDICT: kernel uses block[..end-4] (only det_checksum excluded)");
    } else if m12 {
        eprintln!("VERDICT: kernel uses block[..end-12] (full tail excluded)");
    } else {
        eprintln!("VERDICT: NEITHER matched — different recipe entirely");
    }

    // Don't assert either way — this is a probe. Human reads the output.
    let _ = linux_crc32c; // silence unused
}
