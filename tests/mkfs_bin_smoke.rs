//! Smoke test for the `mkfs.ext4` (mkfs_ext4) binary.
//!
//! Pre-creates a 32 MiB regular file, runs the binary against it with a
//! known label + UUID, then re-opens the file via the crate's own mount
//! path and verifies the on-disk layout the binary produced is parseable
//! and reflects the CLI args. Catches:
//!   - args plumbed to format_filesystem() correctly (label, UUID propagate)
//!   - file-as-device path opens R/W under the binary's process
//!   - resulting bytes mount cleanly without corruption
//!
//! Stays in tests/ rather than examples/ so `cargo test` runs it as part
//! of the standard suite. The test does NOT require any external tool —
//! it's a pure crate-internal round trip. The matching CI workflow
//! against real `fsck.ext4` from e2fsprogs lives in the parent repo's
//! GitHub Actions config.

use fs_ext4::block_io::FileDevice;
use fs_ext4::fs::Filesystem;
use std::process::Command;
use std::sync::Arc;

const SIZE_BYTES: u64 = 32 * 1024 * 1024;
const TEST_LABEL: &str = "BINSMOKE";
const TEST_UUID: &str = "deadbeef-cafe-1234-5678-0123456789ab";

fn unique_tmp_path(suffix: &str) -> std::path::PathBuf {
    // pid + nanos so parallel `cargo test` runs don't clobber each other.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("fs-ext4-mkfs-bin-{pid}-{nanos}-{suffix}"))
}

#[test]
fn mkfs_bin_formats_a_pre_sized_file_and_mounts_clean() {
    let bin = env!("CARGO_BIN_EXE_mkfs_ext4");
    let img = unique_tmp_path("img");
    let img_str = img.to_string_lossy().into_owned();

    // Pre-size with std (no `truncate` shell-out — keeps the test
    // platform-portable for when this runs on Windows CI later).
    {
        let f = std::fs::File::create(&img).expect("create img");
        f.set_len(SIZE_BYTES).expect("set_len");
    }

    // Run: mkfs_ext4 -L BINSMOKE -U <uuid> <img>
    let out = Command::new(bin)
        .args(["-L", TEST_LABEL, "-U", TEST_UUID, &img_str])
        .output()
        .expect("spawn mkfs_ext4");

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "mkfs_ext4 failed: status={:?}\nstderr:\n{stderr}",
            out.status
        );
    }

    // Mount the result via our own read path. If the binary wrote a
    // malformed superblock / BGD / root inode, this will fail.
    let dev = FileDevice::open(&img_str).expect("open formatted image");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount formatted image");

    // Verify args propagated. Label is stored in the superblock's 16-byte
    // volume_name field; check the prefix matches what we passed in.
    assert!(
        fs.sb.volume_name.starts_with(TEST_LABEL),
        "expected volume_name to start with {TEST_LABEL:?}, got {:?}",
        fs.sb.volume_name
    );

    // UUID round-trip — bytes should match the parsed-from-CLI hex string.
    let expected_uuid: [u8; 16] = [
        0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0x12, 0x34, 0x56, 0x78, 0x01, 0x23, 0x45, 0x67, 0x89,
        0xab,
    ];
    assert_eq!(
        fs.sb.uuid, expected_uuid,
        "UUID mismatch — CLI -U argument did not propagate to superblock"
    );

    // Block size defaulted to 4096 (we didn't pass -b).
    assert_eq!(fs.sb.block_size(), 4096);

    // Best-effort cleanup. If this fails (e.g. a preceding panic killed
    // the test before reaching here) the temp file just lingers — fine,
    // the OS reclaims temp_dir eventually and the unique path means no
    // collision next run.
    let _ = std::fs::remove_file(&img);
}

#[test]
fn mkfs_bin_dry_run_does_not_modify_file() {
    let bin = env!("CARGO_BIN_EXE_mkfs_ext4");
    let img = unique_tmp_path("dryrun");
    let img_str = img.to_string_lossy().into_owned();

    // Pre-fill with a recognisable byte pattern — anything other than the
    // ext4 superblock magic at offset 1080 would do, but 0xAA is loud.
    let pattern = vec![0xAAu8; SIZE_BYTES as usize];
    std::fs::write(&img, &pattern).expect("seed pattern");

    let out = Command::new(bin)
        .args(["-n", "-L", "DRYRUN", &img_str])
        .output()
        .expect("spawn mkfs_ext4 -n");
    assert!(
        out.status.success(),
        "dry-run mkfs_ext4 should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // File contents must be unchanged.
    let after = std::fs::read(&img).expect("read after dry-run");
    assert_eq!(
        after.len(),
        pattern.len(),
        "dry-run must not change file size"
    );
    assert!(
        after == pattern,
        "dry-run must not modify file contents (first diff somewhere)"
    );

    let _ = std::fs::remove_file(&img);
}
