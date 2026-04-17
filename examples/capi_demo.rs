//! Minimal self-contained example exercising the ext4rs_* C ABI.
//!
//! Doubles as onboarding material for anyone touching the Rust↔Swift bridge:
//! mount a file-backed ext4 image, print volume info, walk the root dir,
//! stat /test.txt, read its contents, and exit. Mirrors the pattern the
//! Swift FSKit extension uses in ext4fskitd/EXT4Backend.swift.
//!
//! Run with:
//!   cargo run --example capi_demo -- path/to/image.img

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test-disks/ext4-basic.img".into());

    let c_path = CString::new(path.clone()).expect("CString");
    let fs = unsafe { ext4rs_mount(c_path.as_ptr()) };
    if fs.is_null() {
        eprintln!("mount failed: {}", last_err());
        std::process::exit(1);
    }

    // Volume info.
    let mut info: ext4rs_volume_info_t = unsafe { std::mem::zeroed() };
    if unsafe { ext4rs_get_volume_info(fs, &mut info) } == 0 {
        let name_bytes: Vec<u8> =
            info.volume_name.iter().take_while(|&&b| b != 0).map(|&b| b as u8).collect();
        let name = String::from_utf8_lossy(&name_bytes);
        println!(
            "mounted {path}: label={name:?} bs={} blocks={} free={} inodes={} free_inodes={}",
            info.block_size, info.total_blocks, info.free_blocks,
            info.total_inodes, info.free_inodes
        );
    }

    // Root directory listing.
    let slash = CString::new("/").unwrap();
    let iter = unsafe { ext4rs_dir_open(fs, slash.as_ptr()) };
    if iter.is_null() {
        eprintln!("dir_open(/) failed: {}", last_err());
    } else {
        println!("root entries:");
        loop {
            let e = unsafe { ext4rs_dir_next(iter) };
            if e.is_null() { break; }
            let entry = unsafe { &*e };
            let name_bytes: Vec<u8> = entry.name[..entry.name_len as usize]
                .iter()
                .map(|b| *b as u8)
                .collect();
            println!(
                "  ino={:<8} ft={} {}",
                entry.inode,
                entry.file_type,
                String::from_utf8_lossy(&name_bytes)
            );
        }
        unsafe { ext4rs_dir_close(iter) };
    }

    // stat + read /test.txt if it exists.
    let file = CString::new("/test.txt").unwrap();
    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    if unsafe { ext4rs_stat(fs, file.as_ptr(), &mut attr) } == 0 {
        println!(
            "/test.txt: ino={} size={} mode=0o{:o} mtime={}",
            attr.inode, attr.size, attr.mode, attr.mtime
        );
        let mut buf = vec![0u8; attr.size.min(4096) as usize];
        let n = unsafe {
            ext4rs_read_file(
                fs,
                file.as_ptr(),
                buf.as_mut_ptr() as *mut c_void,
                0,
                buf.len() as u64,
            )
        };
        if n > 0 {
            buf.truncate(n as usize);
            println!("  contents: {:?}", String::from_utf8_lossy(&buf));
        } else if n < 0 {
            eprintln!("  read_file failed: {}", last_err());
        }
    } else {
        println!("/test.txt not present on this image");
    }

    unsafe { ext4rs_umount(fs) };
}

fn last_err() -> String {
    unsafe {
        let p = ext4rs_last_error();
        if p.is_null() { return "<null>".into(); }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
