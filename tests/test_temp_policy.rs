//! Where scratch files go, and why there is only one answer.
//!
//! The oracle tools run inside the fs-linux-test-harness VM, which sees
//! this repository at the path the host knows it by and nothing else of
//! the host. An image under `/tmp`, or under `$RUNNER_TEMP` on CI, is a
//! path `e2fsck` cannot open when it is asked to read it. So the scratch
//! root is inside the repository on every machine, and a caller that
//! names one outside it is refused rather than left to fail later with
//! "No such file or directory" in a guest.

use fs_ext4_test_support::{materialize_temp_dir, select_temp_dir};
use std::ffi::OsStr;
use std::path::Path;

#[test]
fn scratch_lives_in_the_repository_by_default() {
    let worktree = Path::new("/worktree");
    assert_eq!(select_temp_dir(None, worktree), worktree.join("tmp"));
    assert_eq!(
        select_temp_dir(Some(OsStr::new("")), worktree),
        worktree.join("tmp"),
        "an empty FS_EXT4_TEST_TMPDIR is no choice at all"
    );
}

#[test]
fn an_explicit_directory_inside_the_repository_is_taken_exactly() {
    let worktree = Path::new("/worktree");
    assert_eq!(
        select_temp_dir(Some(OsStr::new("/worktree/scratch/run-1")), worktree),
        Path::new("/worktree/scratch/run-1")
    );
}

#[test]
#[should_panic(expected = "which is outside")]
fn an_explicit_directory_outside_the_repository_is_refused() {
    select_temp_dir(Some(OsStr::new("/tmp/elsewhere")), Path::new("/worktree"));
}

#[test]
fn every_non_explicit_root_creates_a_unique_child() {
    let base = fs_ext4_test_support::temp_dir()
        .join(format!("fs-ext4-temp-policy-test.{}", std::process::id()));
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
    let exact = fs_ext4_test_support::temp_dir().join(format!(
        "fs-ext4-explicit-policy-test.{}",
        std::process::id()
    ));
    let selected = materialize_temp_dir(Some(OsStr::new("configured")), &exact)
        .expect("create exact directory");

    assert_eq!(selected, exact);
    std::fs::remove_dir(&selected).expect("remove exact directory");
}
