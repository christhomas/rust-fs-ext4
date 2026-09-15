use fs_ext4_test_support::{materialize_temp_dir, select_temp_dir};
use std::ffi::OsStr;
use std::path::Path;

#[test]
fn scratch_location_follows_explicit_ci_pi_then_platform_policy() {
    let worktree = Path::new("/worktree");
    let platform = Path::new("/platform/tmp");

    assert_eq!(
        select_temp_dir(
            Some(OsStr::new("/explicit/nvme")),
            Some(OsStr::new("/managed/base")),
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
            Some(OsStr::new("/managed/base")),
            true,
            Some(OsStr::new("/runner/tmp")),
            None,
            worktree,
            platform
        ),
        Path::new("/managed/base")
    );
    assert_eq!(
        select_temp_dir(
            None,
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
        select_temp_dir(None, None, true, None, None, worktree, platform),
        platform
    );
    assert_eq!(
        select_temp_dir(
            None,
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

#[test]
fn every_non_explicit_root_creates_a_unique_child() {
    let base =
        std::env::temp_dir().join(format!("fs-ext4-temp-policy-test.{}", std::process::id()));
    let first = materialize_temp_dir(None, &base).expect("first managed child");
    let second = materialize_temp_dir(None, &base).expect("second managed child");

    assert_eq!(first.parent(), Some(base.as_path()));
    assert_eq!(second.parent(), Some(base.as_path()));
    assert_ne!(first, second);

    std::fs::remove_dir_all(&first).expect("remove first managed child");
    std::fs::remove_dir_all(&second).expect("remove second managed child");
    std::fs::remove_dir(&base).expect("remove test base");
}

#[test]
fn explicit_directory_is_preserved_exactly() {
    let exact = std::env::temp_dir().join(format!(
        "fs-ext4-explicit-policy-test.{}",
        std::process::id()
    ));
    let selected = materialize_temp_dir(Some(OsStr::new("configured")), &exact)
        .expect("create exact directory");

    assert_eq!(selected, exact);
    std::fs::remove_dir(&selected).expect("remove exact directory");
}
