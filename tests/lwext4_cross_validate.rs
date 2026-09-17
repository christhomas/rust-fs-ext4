//! Cross-validate fs-ext4 images against lwext4 (BSD-2-Clause).
//!
//! **Opt-in.** This harness is silently skipped unless the env var
//! `LWEXT4_DIR` points at a built lwext4 source tree. The intent: dev
//! machines run `cargo test` without needing a C compiler + lwext4
//! checkout, while a dedicated CI lane (or
//! `scripts/cross-validate-lwext4.sh`) explicitly opts in.
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
//! Phase A status: the env-var-gated skeleton lands here so the test
//! contract is recorded and `scripts/cross-validate-lwext4.sh` has a
//! target to invoke. The full diff machinery is a focused follow-up
//! once a lwext4 build is committed to either CI or a dev's local box.
//!
//! Spec source: github.com/gkostka/lwext4 (BSD-2-Clause).

use std::path::PathBuf;

/// Resolve the lwext4 build dir from env. Returns `None` (test skipped)
/// when not set or not pointing at a built tree.
fn lwext4_dir() -> Option<PathBuf> {
    let raw = std::env::var("LWEXT4_DIR").ok()?;
    let p = PathBuf::from(raw);
    // Existence check on the static lib — proves we're pointing at a
    // built tree, not just an empty checkout. The script
    // (`cross-validate-lwext4.sh`) builds this artifact via `make
    // generic` before invoking the test.
    let lib = p.join("build_generic/src/liblwext4.a");
    if lib.exists() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn lwext4_cross_validate_skips_when_lwext4_dir_unset() {
    // Self-test of the gating logic. Always passes; documents the
    // skip contract so a developer running `cargo test` on a stock
    // box understands why no validation actually happens here.
    let dir = lwext4_dir();
    if dir.is_none() {
        eprintln!(
            "[lwext4_cross_validate] SKIP: set LWEXT4_DIR to a built lwext4 tree to enable. \
             Easiest: run `scripts/cross-validate-lwext4.sh` which clones, builds, \
             exports the env var, and re-invokes this test."
        );
        return;
    }
    eprintln!(
        "[lwext4_cross_validate] lwext4 detected at {}",
        dir.as_ref().unwrap().display()
    );
}

#[test]
fn lwext4_cross_validate_each_test_image() {
    match lwext4_dir() {
        None => eprintln!("[lwext4_cross_validate] SKIP (no LWEXT4_DIR)"),
        Some(dir) => cross_validate_each_test_image(&dir),
    }
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
    let fake = std::env::temp_dir().join(format!("lwext4-fake-{}", std::process::id()));
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
