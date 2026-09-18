//! Cross-validate fs-ext4 images against lwext4 (BSD-2-Clause).
//!
//! **Not implemented yet (#99), and ignored rather than skipped.** The
//! comparison itself has not been written, so there is nothing for a
//! default `cargo test` to run. It used to "skip" when `LWEXT4_DIR` was
//! unset — two tests printing SKIP and passing, which reads exactly like
//! validation that happened. It is now one `#[ignore]`d test, counted as
//! ignored in every run, that fails when asked for
//! (`scripts/cross-validate-lwext4.sh` runs it with `--ignored`) until
//! the comparison exists.
//!
//! ## Why a separate impl?
//!
//! lwext4 is an independent BSD-2-Clause C implementation of ext2/3/4
//! that doesn't share a code lineage with either the Linux kernel or
//! this crate. Bugs hidden by ambiguous spec wording (or by
//! "everyone-misreads-it-the-same-way" defects) tend to surface as
//! lwext4-vs-ours divergence — exactly the class of issue our in-tree
//! `verify::verify` cannot catch by construction (it shares our spec
//! interpretation).
//!
//! ## Test contract (full integration — to be implemented incrementally)
//!
//! For each image in `test-disks/*.img`:
//!   1. Mount via fs-ext4. List `/`. Read every regular file.
//!   2. Mount the *same* image via lwext4 (subprocess invocation of
//!      lwext4's `fileapi_demo` binary, or a thin C-FFI wrapper if we
//!      decide to take that on later).
//!   3. Diff the two views: same filenames, same byte content,
//!      same i_size/i_mode for each entry.
//!   4. Report any divergence as the test failure — the message points
//!      at which file diverged and how.
//!
//! Status: the contract is recorded here and
//! `scripts/cross-validate-lwext4.sh` builds lwext4 and invokes the
//! ignored test. The diff machinery is the follow-up (#99); when it
//! lands, lwext4 becomes another oracle the harness VM provides and the
//! `#[ignore]` goes.
//!
//! Spec source: github.com/gkostka/lwext4 (BSD-2-Clause).

/// The one entry point, and it is ignored: see the module header. Asked
/// for explicitly (`--ignored`), it requires `LWEXT4_DIR` — a built
/// lwext4 tree — and then fails, because the comparison does not exist.
#[test]
#[ignore = "lwext4 cross-validation is not implemented (#99); scripts/cross-validate-lwext4.sh runs it with --ignored"]
fn lwext4_cross_validate_each_test_image() {
    let dir = std::env::var_os("LWEXT4_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "[lwext4_cross_validate] LWEXT4_DIR is not set: run \
             `scripts/cross-validate-lwext4.sh`, which builds lwext4 and sets it"
            )
        });
    assert!(
        dir.join("build_generic/src/liblwext4.a").is_file(),
        "[lwext4_cross_validate] LWEXT4_DIR={} is not a built lwext4 tree",
        dir.display()
    );
    cross_validate_each_test_image(&dir);
}

/// The comparison, when a built lwext4 tree is present.
///
/// NOT WRITTEN YET, AND SAYS SO BY FAILING (#99). This body was a comment
/// and a success, so enabling the harness -- setting `LWEXT4_DIR`, which
/// is all `scripts/cross-validate-lwext4.sh` does -- turned a skip that
/// nobody believed was validation into a pass that looked like one.
/// Until it iterates `test-disks/ext*.img`, reads every file through
/// both drivers and compares (path -> size, mode, sha256), asking for it
/// is an error.
///
/// What it has to do, when it is written:
///
/// 1. Iterate `test-disks/ext*.img` and any `LWEXT4_VALIDATE_IMAGE`.
/// 2. For each, run lwext4 over it (its demo binary, or a thin C
///    wrapper) and capture a listing with per-file content hashes.
/// 3. Mount the same image with `Filesystem::mount` and do the same.
/// 4. Compare the two maps; a divergence fails naming the path.
fn cross_validate_each_test_image(dir: &std::path::Path) {
    panic!(
        "[lwext4_cross_validate] LWEXT4_DIR is set ({}), but the lwext4 comparison is not \
         implemented: this would report success having compared nothing. See #99.",
        dir.display()
    );
}

/// Enabling the harness fails while it has nothing to compare, so a CI
/// lane that sets `LWEXT4_DIR` cannot go green on an empty body.
#[test]
fn enabling_the_harness_fails_until_it_compares_something() {
    let fake = fs_ext4_test_support::temp_dir().join(format!("lwext4-fake-{}", std::process::id()));
    std::fs::create_dir_all(fake.join("build_generic/src")).unwrap();
    std::fs::write(fake.join("build_generic/src/liblwext4.a"), b"").unwrap();
    let outcome = std::panic::catch_unwind(|| cross_validate_each_test_image(&fake));
    let _ = std::fs::remove_dir_all(&fake);
    let message = outcome
        .expect_err("an enabled harness with no comparison reported success")
        .downcast::<String>()
        .map(|m| *m)
        .unwrap_or_default();
    assert!(message.contains("not implemented"), "{message}");
}
