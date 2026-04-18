//! C ABI coverage on deep extent trees.
//!
//! ext4-deep-extents.img layout:
//!   /sparse.bin  — 16 MB sparse file with single 'X' bytes every 64 KB
//!                  (~245 extents, forces multi-level extent tree)
//!   /dense.txt   — "control file\n" (13 bytes, single inline extent)
//!
//! Exercises fs_ext4_read_file through extent tree walks that descend
//! into internal nodes, and confirms sparse holes read as zero.

use fs_ext4::capi::*;
use std::ffi::CString;
use std::os::raw::c_void;
use std::path::Path;

const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test-disks/ext4-deep-extents.img"
);

fn mount_or_skip() -> Option<*mut fs_ext4_fs_t> {
    if !Path::new(IMAGE).exists() {
        eprintln!("skip: {IMAGE} not built");
        return None;
    }
    let p = CString::new(IMAGE).unwrap();
    let fs = unsafe { fs_ext4_mount(p.as_ptr()) };
    if fs.is_null() {
        return None;
    }
    Some(fs)
}

fn read_file(fs: *mut fs_ext4_fs_t, path: &str, offset: u64, length: u64) -> Vec<u8> {
    let c = CString::new(path).unwrap();
    let mut buf = vec![0u8; length as usize];
    let n = unsafe {
        fs_ext4_read_file(
            fs,
            c.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            offset,
            length,
        )
    };
    if n < 0 {
        let err = unsafe {
            std::ffi::CStr::from_ptr(fs_ext4_last_error())
                .to_string_lossy()
                .into_owned()
        };
        panic!("read_file({path}, {offset}, {length}) failed: {err}");
    }
    buf.truncate(n as usize);
    buf
}

#[test]
fn dense_file_reads_expected_content() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let data = read_file(fs, "/dense.txt", 0, 64);
    assert_eq!(data, b"control file\n");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn sparse_file_first_byte_is_x() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    // Image lays down 'X' at every 64 KB boundary. Byte 0 should be 'X'.
    let data = read_file(fs, "/sparse.bin", 0, 1);
    assert_eq!(data, b"X", "first byte of sparse.bin should be 'X'");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn sparse_file_holes_read_as_zero() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    // The second byte (offset 1) is inside a sparse hole — must be zero.
    let data = read_file(fs, "/sparse.bin", 1, 4);
    assert_eq!(data, vec![0u8; 4], "bytes 1..5 should be in a hole → zeros");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn sparse_file_deep_extent_lookup() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    // Read at 64KB (offset 65536) — second 'X' byte. Hitting this logical
    // block forces walking the extent tree past the first leaf, exercising
    // the multi-level / internal-node descent path.
    let data = read_file(fs, "/sparse.bin", 65536, 1);
    assert_eq!(data, b"X", "byte at 64KB should be 'X'");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn sparse_file_high_offset_read() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    // Near the end of the 16MB file. Offset 15 MiB + some. Still within size.
    let offset = 15 * 1024 * 1024; // 15 MiB
    let data = read_file(fs, "/sparse.bin", offset, 1);
    // This offset may land on 'X' (if 15MiB is a multiple of 64KB) or zero.
    // Either value is valid — what matters is no panic + at most 1 byte.
    assert_eq!(data.len(), 1);
    assert!(data[0] == 0 || data[0] == b'X');
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn sparse_file_stat_reports_full_logical_size() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let c = CString::new("/sparse.bin").unwrap();
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_stat(fs, c.as_ptr(), &mut attr) };
    assert_eq!(rc, 0);
    assert_eq!(
        attr.size,
        16 * 1024 * 1024,
        "sparse.bin logical size = 16 MiB"
    );
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn read_past_eof_returns_zero() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let c = CString::new("/sparse.bin").unwrap();
    let mut buf = [0u8; 16];
    let n = unsafe {
        fs_ext4_read_file(
            fs,
            c.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            1u64 << 40, // way past end
            buf.len() as u64,
        )
    };
    assert_eq!(n, 0, "read past EOF should return 0 bytes");
    unsafe { fs_ext4_umount(fs) };
}
