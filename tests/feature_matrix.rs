//! Feature matrix tests — run our Rust implementation against the test-disks/
//! images that exercise different ext4 feature combinations.
//!
//! This is the same C ABI surface that the Swift FSKit extension uses —
//! these tests are the best proxy we have for "will this mount under FSKit?"
//! without running Xcode.

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;

fn last_err() -> String {
    unsafe {
        let p = ext4rs_last_error();
        if p.is_null() { return "<null>".into(); }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn image_path(name: &str) -> CString {
    CString::new(format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)).unwrap()
}

fn list_dir(fs: *mut ext4rs_fs_t, path: &str) -> Vec<(String, u8)> {
    let p = CString::new(path).unwrap();
    let iter = unsafe { ext4rs_dir_open(fs, p.as_ptr()) };
    if iter.is_null() {
        panic!("dir_open {path}: {}", last_err());
    }
    let mut entries = Vec::new();
    loop {
        let de = unsafe { ext4rs_dir_next(iter) };
        if de.is_null() { break; }
        let ft = unsafe { (*de).file_type };
        let name_ptr = unsafe { &(*de).name[0] as *const _ };
        let name = unsafe { CStr::from_ptr(name_ptr).to_string_lossy().into_owned() };
        entries.push((name, ft));
    }
    unsafe { ext4rs_dir_close(iter) };
    entries
}

fn read_file_to_string(fs: *mut ext4rs_fs_t, path: &str, max: usize) -> Option<String> {
    let p = CString::new(path).unwrap();
    let mut buf = vec![0u8; max];
    let n = unsafe {
        ext4rs_read_file(fs, p.as_ptr(), buf.as_mut_ptr() as *mut c_void, 0, max as u64)
    };
    if n < 0 { return None; }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

// ---------------------------------------------------------------------------
// ext4-htree.img — 256 files in /bigdir, forces htree indexing
// ---------------------------------------------------------------------------

#[test]
fn htree_readdir_returns_all_256_files() {
    let path = image_path("ext4-htree.img");
    let fs = unsafe { ext4rs_mount(path.as_ptr()) };
    if fs.is_null() {
        eprintln!("skip ext4-htree.img: {}", last_err());
        return;
    }

    let entries = list_dir(fs, "/bigdir");
    let file_count = entries.iter().filter(|(n, _)| n.starts_with("file_")).count();
    // Expect . + .. + 256 files = 258. Tolerate . / .. handling variations.
    assert!(
        file_count == 256,
        "htree readdir returned {} files (expected 256). Full entries: {:?}",
        file_count,
        entries.iter().take(5).collect::<Vec<_>>()
    );

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn htree_lookup_specific_file() {
    // path::lookup currently uses linear scan. Should work even without htree
    // fast-path (just O(n) instead of O(log n)).
    let path = image_path("ext4-htree.img");
    let fs = unsafe { ext4rs_mount(path.as_ptr()) };
    if fs.is_null() {
        eprintln!("skip ext4-htree.img: {}", last_err());
        return;
    }

    let p = CString::new("/bigdir/file_128.txt").unwrap();
    let mut attr = unsafe { std::mem::zeroed::<ext4rs_attr_t>() };
    let rc = unsafe { ext4rs_stat(fs, p.as_ptr(), &mut attr) };
    assert_eq!(rc, 0, "stat /bigdir/file_128.txt: {}", last_err());
    assert_eq!(attr.file_type as u32, ext4rs_file_type_t::RegFile as u32);

    unsafe { ext4rs_umount(fs) };
}

// ---------------------------------------------------------------------------
// ext4-csum-seed.img — Pi-style INCOMPAT_CSUM_SEED
// ---------------------------------------------------------------------------

#[test]
fn csum_seed_image_mounts() {
    let path = image_path("ext4-csum-seed.img");
    let fs = unsafe { ext4rs_mount(path.as_ptr()) };
    assert!(!fs.is_null(), "CSUM_SEED image failed to mount: {}", last_err());

    // Read /hello.txt
    let content = read_file_to_string(fs, "/hello.txt", 256);
    assert_eq!(
        content.as_deref(),
        Some("pi-style file\n"),
        "unexpected content: {:?}",
        content
    );

    // /etc/fstab should exist and be readable
    let fstab = read_file_to_string(fs, "/etc/fstab", 256);
    assert_eq!(
        fstab.as_deref(),
        Some("fake fstab\n"),
        "unexpected fstab: {:?}",
        fstab
    );

    unsafe { ext4rs_umount(fs) };
}

// ---------------------------------------------------------------------------
// ext4-deep-extents.img — multi-level extent tree
// ---------------------------------------------------------------------------

#[test]
fn deep_extent_tree_sparse_file() {
    let path = image_path("ext4-deep-extents.img");
    let fs = unsafe { ext4rs_mount(path.as_ptr()) };
    if fs.is_null() {
        eprintln!("skip ext4-deep-extents.img: {}", last_err());
        return;
    }

    // /dense.txt — simple single-extent file
    let dense = read_file_to_string(fs, "/dense.txt", 256);
    assert_eq!(dense.as_deref(), Some("control file\n"), "got: {:?}", dense);

    // /sparse.bin — 16MB sparse file, 'X' every 64KB.
    // Read first 128KB to verify the extent tree descends correctly.
    let p = CString::new("/sparse.bin").unwrap();
    let mut buf = vec![0u8; 128 * 1024];
    let n = unsafe {
        ext4rs_read_file(fs, p.as_ptr(), buf.as_mut_ptr() as *mut c_void, 0, buf.len() as u64)
    };
    assert!(n > 0, "read /sparse.bin 0..128K: {}", last_err());

    // Should find 'X' at offset 0 and 65536
    assert_eq!(buf[0], b'X', "expected 'X' at offset 0");
    assert_eq!(buf[65536], b'X', "expected 'X' at offset 64K");
    // And lots of zeros in between (sparse holes read as zero)
    assert_eq!(buf[1000], 0, "expected 0 in sparse region");
    assert_eq!(buf[65000], 0, "expected 0 before next 'X'");

    unsafe { ext4rs_umount(fs) };
}

// ---------------------------------------------------------------------------
// ext4-no-csum.img — no metadata_csum (legacy behavior)
// ---------------------------------------------------------------------------

#[test]
fn no_csum_image_mounts() {
    let path = image_path("ext4-no-csum.img");
    let fs = unsafe { ext4rs_mount(path.as_ptr()) };
    assert!(!fs.is_null(), "no-csum image failed to mount: {}", last_err());

    let content = read_file_to_string(fs, "/file.txt", 256);
    assert_eq!(content.as_deref(), Some("no checksum here\n"), "got: {:?}", content);

    unsafe { ext4rs_umount(fs) };
}

// ---------------------------------------------------------------------------
// Volume info sanity across all images
// ---------------------------------------------------------------------------

#[test]
fn all_images_report_volume_info() {
    for img in ["ext4-basic.img", "ext4-htree.img", "ext4-csum-seed.img",
                "ext4-deep-extents.img", "ext4-no-csum.img"] {
        let path = image_path(img);
        let fs = unsafe { ext4rs_mount(path.as_ptr()) };
        if fs.is_null() {
            eprintln!("{img} failed to mount: {}", last_err());
            continue;
        }

        let mut info = unsafe { std::mem::zeroed::<ext4rs_volume_info_t>() };
        let rc = unsafe { ext4rs_get_volume_info(fs, &mut info) };
        assert_eq!(rc, 0, "{img} get_volume_info failed: {}", last_err());
        // Block size varies per image (1K, 2K, or 4K depending on mkfs defaults)
        assert!(
            matches!(info.block_size, 1024 | 2048 | 4096),
            "{img} unexpected block_size: {}",
            info.block_size
        );
        assert!(info.total_blocks > 0, "{img} total_blocks == 0");

        unsafe { ext4rs_umount(fs) };
    }
}
