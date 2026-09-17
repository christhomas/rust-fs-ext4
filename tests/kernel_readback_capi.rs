//! THE KERNEL READS BACK WHAT THE C ABI WROTE.
//!
//! `tests/kernel_readback.rs` drives the Rust API; this one drives
//! `fs_ext4_*`, the interface every consumer actually links against
//! (DiskJockey among them). The same tree, written through the FFI
//! boundary — its own path handling, its own error reporting, its own
//! mount handle — and the same kernel mount reading it back.
//!
//! The two are separate files rather than one parameterised test: a
//! failure here means the C ABI writes something the Rust API does not,
//! and that difference is the whole point of running both.

use fs_ext4::capi::*;
use fs_ext4_test_support::{
    assert_e2fsck_clean, guest_kernel_report, oracle, sha256_hex, temp_path,
};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::raw::c_void;

/// A fresh volume, formatted by the tool in the harness VM.
fn volume(tag: &str) -> String {
    let image = temp_path!("fs_ext4_kernel_capi_{tag}_{}.img", std::process::id());
    let _ = std::fs::remove_file(&image);
    std::fs::File::create(&image)
        .and_then(|f| f.set_len(64 * 1024 * 1024))
        .unwrap_or_else(|e| panic!("size {image}: {e}"));
    let out = oracle("mkfs.ext4")
        .args(["-q", "-F", "-b", "4096"])
        .arg(&image)
        .output();
    assert!(
        out.status.success(),
        "mkfs.ext4 {image}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    image
}

fn payload(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = 0x0fed_cba9_8765_4321u64;
    while out.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn last_error() -> String {
    unsafe {
        std::ffi::CStr::from_ptr(fs_ext4_last_error())
            .to_string_lossy()
            .into_owned()
    }
}

fn c(path: &str) -> CString {
    CString::new(path).expect("a path with no NUL")
}

#[test]
fn the_kernel_reads_back_what_the_c_abi_wrote() {
    let image = volume("tree");
    let big = payload(3 * 1024 * 1024 + 1234);
    let small = b"written through fs_ext4_write_file\n".to_vec();
    let renamed = b"renamed through fs_ext4_rename\n".to_vec();
    let truncated = payload(120_000)[..8193].to_vec();

    unsafe {
        let image_c = c(&image);
        let fs = fs_ext4_mount_rw(image_c.as_ptr());
        assert!(!fs.is_null(), "mount rw: {}", last_error());

        // mkdir and create answer with the new inode number; zero is
        // the failure.
        assert_ne!(
            fs_ext4_mkdir(fs, c("/capi").as_ptr(), 0o755),
            0,
            "{}",
            last_error()
        );
        assert_ne!(
            fs_ext4_mkdir(fs, c("/capi/inner").as_ptr(), 0o2750),
            0,
            "{}",
            last_error()
        );

        let small_path = c("/capi/small.txt");
        assert_ne!(
            fs_ext4_create(fs, small_path.as_ptr(), 0o644),
            0,
            "{}",
            last_error()
        );
        assert_eq!(
            fs_ext4_write_file(
                fs,
                small_path.as_ptr(),
                small.as_ptr() as *const c_void,
                small.len() as u64,
            ),
            small.len() as i64,
            "{}",
            last_error()
        );

        // The multi-megabyte file, written in pieces that do not line up
        // with block boundaries, through fs_ext4_pwrite.
        let big_path = c("/capi/big.bin");
        assert_ne!(
            fs_ext4_create(fs, big_path.as_ptr(), 0o640),
            0,
            "{}",
            last_error()
        );
        let mut at = 0usize;
        for len in [5usize, 4091, 1_000_003, 13, 4096 * 200 + 7] {
            let end = (at + len).min(big.len());
            let wrote = fs_ext4_pwrite(
                fs,
                big_path.as_ptr(),
                big[at..end].as_ptr() as *const c_void,
                (end - at) as u64,
                at as u64,
            );
            // fs_ext4_pwrite answers with the file's NEW SIZE, not the
            // byte count — these writes are sequential, so that is the
            // end of the piece just written.
            assert_eq!(wrote, end as i64, "pwrite at {at}: {}", last_error());
            at = end;
        }
        if at < big.len() {
            let wrote = fs_ext4_pwrite(
                fs,
                big_path.as_ptr(),
                big[at..].as_ptr() as *const c_void,
                (big.len() - at) as u64,
                at as u64,
            );
            assert_eq!(wrote, big.len() as i64, "pwrite tail: {}", last_error());
        }

        assert_ne!(
            fs_ext4_symlink(fs, c("capi/big.bin").as_ptr(), c("/capi-link").as_ptr()),
            0,
            "{}",
            last_error()
        );

        let value = b"through-the-c-abi";
        assert_eq!(
            fs_ext4_setxattr(
                fs,
                small_path.as_ptr(),
                c("user.origin").as_ptr(),
                value.as_ptr() as *const c_void,
                value.len(),
            ),
            0,
            "{}",
            last_error()
        );

        // Rename, unlink, truncate.
        let before = c("/capi/before-rename");
        assert_ne!(
            fs_ext4_create(fs, before.as_ptr(), 0o644),
            0,
            "{}",
            last_error()
        );
        assert_eq!(
            fs_ext4_write_file(
                fs,
                before.as_ptr(),
                renamed.as_ptr() as *const c_void,
                renamed.len() as u64,
            ),
            renamed.len() as i64,
            "{}",
            last_error()
        );
        assert_eq!(
            fs_ext4_rename(fs, before.as_ptr(), c("/capi/inner/after-rename").as_ptr()),
            0,
            "{}",
            last_error()
        );

        let doomed = c("/capi/doomed");
        assert_ne!(
            fs_ext4_create(fs, doomed.as_ptr(), 0o644),
            0,
            "{}",
            last_error()
        );
        assert_eq!(fs_ext4_unlink(fs, doomed.as_ptr()), 0, "{}", last_error());

        let shrunk = c("/capi/shrunk.bin");
        let source = payload(120_000);
        assert_ne!(
            fs_ext4_create(fs, shrunk.as_ptr(), 0o644),
            0,
            "{}",
            last_error()
        );
        assert_eq!(
            fs_ext4_write_file(
                fs,
                shrunk.as_ptr(),
                source.as_ptr() as *const c_void,
                source.len() as u64,
            ),
            source.len() as i64,
            "{}",
            last_error()
        );
        assert_eq!(
            fs_ext4_truncate(fs, shrunk.as_ptr(), 8193),
            0,
            "{}",
            last_error()
        );

        fs_ext4_umount(fs);
    }

    let report = guest_kernel_report(&image, "c abi");
    let mut wrong = Vec::new();
    let mut want = |kind: &str, path: &str, value: String| {
        check(&report, kind, path, &value, &mut wrong);
    };
    want("type", "capi", "directory".into());
    want("mode", "capi", "755".into());
    want("type", "capi/inner", "directory".into());
    // setgid survives the round trip, and `stat -c %a` prints it.
    want("mode", "capi/inner", "2750".into());

    want("size", "capi/small.txt", small.len().to_string());
    want("sha256", "capi/small.txt", sha256_hex(&small));
    want(
        "xattrs",
        "capi/small.txt",
        "user.origin=through-the-c-abi".into(),
    );

    want("mode", "capi/big.bin", "640".into());
    want("size", "capi/big.bin", big.len().to_string());
    want("sha256", "capi/big.bin", sha256_hex(&big));

    want("type", "capi-link", "symbolic-link".into());
    want("target", "capi-link", "capi/big.bin".into());

    want("sha256", "capi/inner/after-rename", sha256_hex(&renamed));
    want("size", "capi/shrunk.bin", "8193".into());
    want("sha256", "capi/shrunk.bin", sha256_hex(&truncated));

    for gone in ["capi/doomed", "capi/before-rename"] {
        if report.contains_key(&("type".to_string(), gone.to_string())) {
            wrong.push(format!("{gone} is still there after the C ABI removed it"));
        }
    }
    assert!(
        wrong.is_empty(),
        "the kernel read back something else than the C ABI wrote:\n{}",
        wrong.join("\n")
    );

    assert_e2fsck_clean(&image, "c abi");
    let _ = std::fs::remove_file(&image);
}

fn check(
    report: &BTreeMap<(String, String), String>,
    kind: &str,
    path: &str,
    value: &str,
    wrong: &mut Vec<String>,
) {
    match report.get(&(kind.to_string(), path.to_string())) {
        Some(got) if got == value => {}
        Some(got) => wrong.push(format!(
            "{kind} of {path}: kernel says {got:?}, wrote {value:?}"
        )),
        None => wrong.push(format!("{kind} of {path}: the kernel did not report it")),
    }
}
