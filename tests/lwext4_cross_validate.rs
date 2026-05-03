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
    let Some(_dir) = lwext4_dir() else {
        eprintln!("[lwext4_cross_validate] SKIP (no LWEXT4_DIR)");
        return;
    };
    // FUTURE WORK (tracked in scripts/cross-validate-lwext4.sh):
    //
    // 1. Iterate `test-disks/ext*.img` and any LWEXT4_VALIDATE_IMAGE override.
    // 2. For each: spawn lwext4's CLI demo as a subprocess, capture its
    //    listing + per-file content hashes.
    // 3. Mount via Filesystem::mount, do the same.
    // 4. assert_eq! on (path → sha256(content)) maps. Any divergence is a
    //    test failure naming the divergent path.
    //
    // The demo binary's exact name + arg format depends on the lwext4
    // build flavor (`generic` vs `xilinx`); the script normalizes this
    // before invoking the test. Once that's settled and we've captured
    // a working invocation in the script, this body fills in.
    //
    // Until then this test passes (intentionally) — it serves as the
    // hook the script plugs into so the integration is incremental, not
    // big-bang.
    eprintln!(
        "[lwext4_cross_validate] env detected; full diff harness pending. \
         Update this body when lwext4 demo binary's invocation is settled."
    );
}
