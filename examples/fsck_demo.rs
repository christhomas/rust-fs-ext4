//! Minimal example exercising `fs_ext4_fsck_run` through the C ABI.
//!
//! Mounts a file-backed ext4 image, runs the read-only fsck, and prints
//! progress + findings as they are emitted. Mirrors the call shape the
//! Swift FSKit extension uses to drive a "verify" pass from the host UI.
//!
//! Run with:
//!   cargo run --example fsck_demo -- path/to/image.img

use fs_ext4::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;

unsafe extern "C" fn on_progress(
    _ctx: *mut c_void,
    _phase: fs_ext4_fsck_phase_t,
    phase_name: *const std::os::raw::c_char,
    done: u64,
    total: u64,
) {
    let name = if phase_name.is_null() {
        ""
    } else {
        CStr::from_ptr(phase_name).to_str().unwrap_or("")
    };
    println!("[progress] {name:>10}: {done}/{total}");
}

unsafe extern "C" fn on_finding(
    _ctx: *mut c_void,
    kind: *const std::os::raw::c_char,
    inode: u32,
    detail: *const std::os::raw::c_char,
) {
    let kind = if kind.is_null() {
        ""
    } else {
        CStr::from_ptr(kind).to_str().unwrap_or("")
    };
    let detail = if detail.is_null() {
        ""
    } else {
        CStr::from_ptr(detail).to_str().unwrap_or("")
    };
    println!("[finding ] kind={kind} inode={inode} detail={detail}");
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test-disks/ext4-basic.img".into());

    let c_path = CString::new(path.clone()).expect("CString");
    let fs = unsafe { fs_ext4_mount(c_path.as_ptr()) };
    if fs.is_null() {
        let err = unsafe { CStr::from_ptr(fs_ext4_last_error()) };
        eprintln!("mount {path} failed: {}", err.to_string_lossy());
        std::process::exit(1);
    }

    let opts = fs_ext4_fsck_options_t {
        read_only: 1,
        replay_journal: 0,
        max_dirs: 0,
        max_entries_per_dir: 0,
        on_progress: Some(on_progress),
        on_finding: Some(on_finding),
        context: std::ptr::null_mut(),
    };

    let mut report: fs_ext4_fsck_report_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_fsck_run(fs, &opts, &mut report) };
    if rc != 0 {
        let err = unsafe { CStr::from_ptr(fs_ext4_last_error()) };
        eprintln!("fsck failed: {}", err.to_string_lossy());
        unsafe { fs_ext4_umount(fs) };
        std::process::exit(1);
    }

    println!(
        "fsck done: dirs={} entries={} inodes={} anomalies={} was_dirty={}",
        report.directories_scanned,
        report.entries_scanned,
        report.inodes_visited,
        report.anomalies_found,
        report.was_dirty
    );

    unsafe { fs_ext4_umount(fs) };
}
