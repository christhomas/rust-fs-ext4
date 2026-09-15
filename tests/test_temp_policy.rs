use fs_ext4_test_support::select_temp_dir;
use std::ffi::OsStr;
use std::path::Path;

#[test]
fn scratch_location_follows_explicit_ci_pi_then_platform_policy() {
    let worktree = Path::new("/worktree");
    let platform = Path::new("/platform/tmp");

    assert_eq!(
        select_temp_dir(
            Some(OsStr::new("/explicit/nvme")),
            true,
            Some(OsStr::new("/runner/tmp")),
            Some(b"Raspberry Pi 5 Model B"),
            worktree,
            platform,
        ),
        Path::new("/explicit/nvme")
    );
    assert_eq!(
        select_temp_dir(
            None,
            true,
            Some(OsStr::new("/runner/tmp")),
            Some(b"Generic ARM Server"),
            worktree,
            platform
        ),
        Path::new("/runner/tmp")
    );
    assert_eq!(
        select_temp_dir(None, true, None, None, worktree, platform),
        platform
    );
    assert_eq!(
        select_temp_dir(
            None,
            false,
            None,
            Some(b"Raspberry Pi 5 Model B Rev 1.1\0"),
            worktree,
            platform,
        ),
        worktree.join("tmp")
    );
    assert_eq!(
        select_temp_dir(
            None,
            false,
            None,
            Some(b"Generic ARM Server"),
            worktree,
            platform
        ),
        platform
    );
}
