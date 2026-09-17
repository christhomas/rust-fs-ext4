//! The debug run that lets the PR gate see an overflow guards itself.
//!
//! `overflow-checks` is on in debug and off in release, so a defect
//! whose only symptom is an arithmetic overflow panic cannot be
//! observed by a release-only test run. This repository already runs a
//! debug suite -- `release.yml:59`, `cargo test --locked --all-targets`
//! -- and that is NOT the same fact as the pull-request gate being able
//! to see the defect: `release.yml` triggers on a version tag, after
//! the change has already merged. A wrapping bug merges green here and
//! surfaces only when someone else cuts the next release, detached from
//! the change and the person who could have caught it.
//!
//! So `ci.yml` -- the workflow that actually gates a merge -- needs its
//! own debug run, and this file is what keeps it there. It checks
//! `ci.yml` specifically and is not satisfied by `release.yml` having
//! one; see `a_debug_run_in_release_yml_alone_does_not_satisfy_the_gate`
//! for that distinction pinned as a test rather than left as a comment
//! someone could stop believing.
//!
//! # Why this is an integration test and not a module under `src/`
//!
//! Cargo discovers `tests/*.rs` on its own, so there is no declaration
//! anywhere that can be deleted to switch this off. A guard living as a
//! file under `src/` behind a `#[cfg(test)] mod` line has no such
//! protection: lose the one line and the file stays, compiles into
//! nothing, and asserts nothing, with no lint to say so. That happened
//! once already on a sibling repository's version of this fix.
//!
//! # The other half: does the debug run actually ask anything
//!
//! `ci.yml` quoting a debug command inside the comment explaining it
//! (see the comment above the step this file is pinning) means a scan
//! that ignored comments would keep passing after the step itself was
//! deleted. And a debug step that compiles but never checks anything is
//! costing a compile for nothing, so the scan also requires the
//! `EXPECT_OVERFLOW_CHECKS` handshake that arms
//! `overflow_checks::the_build_the_gate_asked_to_check_does_check` in
//! `src/lib.rs` -- the runtime half that actually performs an overflow
//! and fails if the build let it through. This file proves the step is
//! present and asked to check; it cannot prove the build can see the
//! overflow, which is what the runtime test is for. Neither is
//! redundant with the other: delete the step and the runtime test never
//! runs at all; keep the step but drop the variable and the runtime
//! test runs, finds nothing to check, and passes doing nothing.

use saphyr::{LoadableYamlNode, Yaml};
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read a file the guard depends on, or fail.
///
/// Panics rather than returning `None` on purpose: a version of this
/// that skipped when the file was missing would reproduce the exact
/// blindness the guard exists to prevent.
fn read_or_panic(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. This guard must fail rather than skip: a \
             version of it that returned early here would be the same \
             blindness it exists to prevent.",
            path.display()
        )
    })
}

/// Every `cargo test` invocation in a workflow that would be compiled
/// with overflow checks on AND asked to check for one.
///
/// Three things disqualify a line:
///
/// - it is a YAML comment. Load-bearing here, not defensive: `ci.yml`
///   quotes the debug command verbatim in the comment above the step
///   that runs it;
/// - it is an inline trailing comment on an otherwise-`--release` line;
/// - it passes `--release`, or names a profile explicitly.
///
/// And the run must carry `EXPECT_OVERFLOW_CHECKS=1` -- a debug step
/// that never asks the build anything buys nothing over deleting it.
fn checking_debug_runs(script: &str) -> Vec<String> {
    script
        .lines()
        .filter_map(|raw| {
            let line = raw.trim_start();
            if line.starts_with('#') {
                return None;
            }
            let command = line.split(" #").next().unwrap_or(line).trim();
            if !command.contains("cargo test") {
                return None;
            }
            if command.contains("--release")
                || command.contains("--profile")
                || selects_release_by_short_flag(command)
            {
                return None;
            }
            if !command.contains("EXPECT_OVERFLOW_CHECKS=1") {
                return None;
            }
            Some(command.to_string())
        })
        .collect()
}

/// Whether `command` runs `cargo test` with the release profile selected
/// by its short flag (#158).
///
/// `cargo test -r` is `cargo test --release`, and the string check above
/// does not see it. Nor is it one spelling: clap merges short flags, so
/// `-qr` and `-rq` carry it as well, anywhere before `--`. A cluster ends
/// at a short option that takes a value -- `-p`, `-j`, `-F` or `-Z` --
/// whose value is the rest of the word or, when the word ends there, the
/// next one: `-j4 -r` is release, `-pr` names a package `r`. Everything
/// after `--` belongs to the test harness, where `-r` is not cargo's.
fn selects_release_by_short_flag(command: &str) -> bool {
    shell_commands(command).iter().any(|command| {
        let words: Vec<&str> = command.iter().map(String::as_str).collect();
        (0..words.len()).any(|at| {
            // `cargo` by name or by path, then any `+toolchain`, then `test`.
            let is_cargo = words[at] == "cargo" || words[at].ends_with("/cargo");
            let mut next = at + 1;
            while is_cargo && words.get(next).is_some_and(|w| w.starts_with('+')) {
                next += 1;
            }
            is_cargo && words.get(next) == Some(&"test") && release_in(&words[next + 1..])
        })
    })
}

/// `command` split into the commands the shell's control operators --
/// `&&`, `||`, `;`, `|` and `&` -- separate, each as its words, with
/// quotes and backslashes removed as the shell removes them.
///
/// Operators need no spaces: `true&&cargo test -r` is a `cargo test` run,
/// and in `cargo test --lib&&rm -rf build` the `-rf` is `rm`'s. An `&` or
/// `|` straight after `>` or `<` is part of a redirection (`2>&1`, `>|`).
/// Inside quotes, or after a backslash, nothing is an operator or a word
/// break, and what the quotes held is the word: `--features 'a;b' -r` is
/// one command, and `'-r'` is `-r`.
fn shell_commands(command: &str) -> Vec<Vec<String>> {
    let mut commands = vec![Vec::new()];
    let mut word: Option<String> = None;
    let mut chars = command.chars().peekable();
    let mut previous = None;
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                word.extend(chars.by_ref().take_while(|&q| q != '\''));
            }
            '"' => {
                let word = word.get_or_insert_with(String::new);
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                            word.extend(chars.next());
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
            ';' | '&' | '|' if !matches!(previous, Some('>' | '<')) => {
                if c != ';' && chars.peek() == Some(&c) {
                    chars.next();
                }
                commands.last_mut().unwrap().extend(word.take());
                commands.push(Vec::new());
            }
            c if c.is_whitespace() => commands.last_mut().unwrap().extend(word.take()),
            c => word.get_or_insert_with(String::new).push(c),
        }
        previous = Some(c);
    }
    commands.last_mut().unwrap().extend(word.take());
    commands
}

/// Whether `cargo test`'s `arguments`, up to the end of its own command,
/// carry `-r`. See [`selects_release_by_short_flag`].
///
/// `arguments` are one command's words ([`shell_commands`]), so in
/// `cargo test --lib && rm -rf build` the `r` in `-rf` is not among them.
fn release_in(arguments: &[&str]) -> bool {
    const LONG_OPTIONS_TAKING_A_VALUE: [&str; 15] = [
        "--package",
        "--exclude",
        "--features",
        "--target",
        "--target-dir",
        "--manifest-path",
        "--profile",
        "--test",
        "--bin",
        "--example",
        "--bench",
        "--jobs",
        "--message-format",
        "--color",
        "--config",
    ];
    let mut next_is_a_value = false;
    for &argument in arguments {
        if std::mem::take(&mut next_is_a_value) {
            continue;
        }
        if argument == "--" {
            return false;
        }
        if argument.starts_with("--") {
            next_is_a_value =
                !argument.contains('=') && LONG_OPTIONS_TAKING_A_VALUE.contains(&argument);
        } else if let Some(cluster) = argument.strip_prefix('-') {
            for (at, flag) in cluster.char_indices() {
                match flag {
                    'r' => return true,
                    'p' | 'j' | 'F' | 'Z' => {
                        next_is_a_value = at + 1 == cluster.len();
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    false
}

/// A workflow, structured just far enough to answer one question:
/// does this step's result actually gate a pull request?
///
/// The line-based scan above finds the command. It cannot see the
/// step's sibling keys, so `if: false` and `continue-on-error: true`
/// left every guard test green while the gate stopped gating -- a step
/// that runs and whose result nothing reads, which is this project's
/// own named defect, committed inside the guard written to prevent it.
///
/// The conditions are ENUMERATED rather than patched one defeat at a
/// time, because twice on this shape the defeat lived in what the scan
/// does not look at rather than in what it compares. A `run:` step
/// gates a pull request only if the step carries no `if:` and no
/// `continue-on-error:`, its job carries neither either, and the
/// workflow still triggers on `pull_request`.
///
/// `if:` and `continue-on-error:` are rejected on the KEY'S PRESENCE,
/// not by evaluating it. `if: false`, `if: ${{ false }}` and an `if:`
/// on an expression that happens to be false are distinct spellings,
/// and four spellings of one manifest key had already defeated a
/// matcher on a sibling repository -- enumerating them is the losing
/// game. Over-strict is the safe direction: a step that genuinely
/// needs a condition can be split out, whereas a guard that
/// interprets conditions acquires a new defeat whenever the syntax
/// grows.
///
/// A step's other keys -- `name:`, `env:`, `uses:`/`with:` -- say
/// nothing about whether the result is read, so they are ACCEPTED. An
/// `env:` mapping in particular must not disqualify a step: that would
/// be over-strictness in the one direction that costs something, since
/// the handshake this guard looks for is itself an environment
/// variable and a maintainer may reasonably move it into a mapping.
///
/// # WHY THIS IS PARSED AND NO LONGER SCANNED
///
/// The version this replaces hand-rolled the YAML, and it was correct
/// only in the sense that it had been patched five times. Each patch
/// was a helper taught one more piece of ordinary grammar:
///
/// ```text
///   without_comment        a trailing `#`, so a commented-out trigger
///                          stopped counting as a trigger
///   key_of                 quotes, so `"if": false` stopped being a
///                          different key from `if: false`
///   opens_a_block_scalar   `|-`, `|+`, `>`, `>-`, `>+`, `|2`, `>2-`,
///                          so a block's contents were read at all
///   indent_of              the block structure itself
///   triggers: Vec<String>  whole names, so `pull_request_review` and
///                          `pull_request` stopped being the same
/// ```
///
/// Every one of those is a rule a YAML parser already has. And the
/// cost of learning them by hand is recorded in this file, twice over:
/// `key_of`'s own comment noted that the identical quote-normalisation
/// had already been added to `profiles_disabling_overflow_checks` a
/// few dozen lines above, after a quoted `"overflow-checks" = false`
/// defeated that scan -- the lesson did not travel between two parsers
/// in one file. Learning it a sixth time was the alternative to this.
///
/// The properties those helpers defended are not dropped with them.
/// Each is now asserted in `mod gating` against the parser instead:
/// every block scalar style is read whole, a quoted key is the same
/// key, a commented-out trigger is not a trigger, and a `#` inside a
/// quoted shell value is content rather than a comment.
///
/// `saphyr` is a dev-dependency, so nothing here reaches a consumer of
/// the crate.
#[derive(Debug)]
struct Step {
    keys: Vec<String>,
    run: String,
}

#[derive(Debug)]
struct Job {
    keys: Vec<String>,
    steps: Vec<Step>,
}

#[derive(Debug)]
struct Workflow {
    /// The trigger NAMES, parsed. Not the `on:` block's text: a
    /// substring search over that text answered `true` for
    /// `pull_request_review:` and for `pull_request` sitting inside a
    /// comment, so the guard reported pull-request coverage that was
    /// not there. Neither spelling needs an adversarial author.
    triggers: Vec<String>,
    jobs: Vec<Job>,
}

/// The value of `name` in a YAML mapping, or `None`.
///
/// By name rather than by constructing a key, because `saphyr`'s `Yaml`
/// borrows the source text and building one to hand to `get` is more
/// ceremony than the lookup is worth here.
fn field<'a, 'b>(node: &'a Yaml<'b>, name: &str) -> Option<&'a Yaml<'b>> {
    node.as_mapping()?
        .iter()
        .find(|(key, _)| key.as_str() == Some(name))
        .map(|(_, value)| value)
}

/// The keys of a YAML mapping, as plain strings.
///
/// The parser has already resolved the quoting, so `"if"`, `'if'` and
/// `if` all arrive here as `if`. That is the whole of what `key_of`
/// did: there is no un-quoting step left to forget.
fn keys_of(node: &Yaml) -> Vec<String> {
    node.as_mapping()
        .map(|mapping| {
            mapping
                .iter()
                .filter_map(|(key, _)| key.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Structure a workflow far enough to answer the questions above.
///
/// Panics on a workflow it cannot parse, deliberately. A guard that
/// returned an empty `Workflow` for a file it did not understand would
/// report "no debug run gates this" -- a failure, so that direction is
/// safe -- but one that returned early with a PASS would be the
/// blindness this module exists to prevent. Failing on the parse error
/// names the real problem instead of a consequence of it.
fn parse_workflow(text: &str) -> Workflow {
    let documents = Yaml::load_from_str(text).unwrap_or_else(|e| {
        panic!(
            "workflow is not valid YAML: {e}. This guard reads the workflow \
             rather than scanning its text, so a file it cannot parse is a \
             failure and never a pass."
        )
    });
    let Some(document) = documents.first() else {
        return Workflow {
            triggers: Vec::new(),
            jobs: Vec::new(),
        };
    };

    // `on:` takes three legal shapes: a mapping of trigger names, a
    // sequence of them, or a single scalar. All three are names.
    //
    // Note that `on` survives as the string key `on` and is not folded
    // into the boolean `true` -- saphyr implements the YAML 1.2 core
    // schema, where only `true`/`false` are booleans. The YAML 1.1
    // reading that would break every GitHub workflow ever written does
    // not apply.
    let triggers = match field(document, "on") {
        Some(on) if on.as_mapping().is_some() => keys_of(on),
        Some(on) if on.as_sequence().is_some() => on
            .as_sequence()
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(on) => on.as_str().map(str::to_string).into_iter().collect(),
        None => Vec::new(),
    };

    let mut jobs = Vec::new();
    if let Some(mapping) = field(document, "jobs").and_then(Yaml::as_mapping) {
        for (_, body) in mapping.iter() {
            let steps = field(body, "steps")
                .and_then(Yaml::as_sequence)
                .into_iter()
                .flatten()
                .map(|step| Step {
                    keys: keys_of(step),
                    // A `run:` block of any style -- `|`, `|-`, `|+`,
                    // `>`, `>-`, `>+`, `|2`, `>2-` -- arrives as one
                    // string with the block folded per its own rules,
                    // so a command inside a shell loop is seen whole
                    // rather than as fragments, and no style is
                    // mistaken for the command itself. That is what
                    // `opens_a_block_scalar` enumerated by hand.
                    run: field(step, "run")
                        .and_then(Yaml::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect();
            jobs.push(Job {
                keys: keys_of(body),
                steps,
            });
        }
    }

    Workflow { triggers, jobs }
}

/// Does this workflow still run on a pull request at all?
///
/// The assumption the `ci.yml`-only scope rests on, and a fact about
/// the file rather than a given: if the triggers stop including
/// `pull_request`, the step gates nothing however it looks.
///
/// MATCHED WHOLE, against parsed trigger names. A substring search
/// over the `on:` block's text answered `true` for
/// `pull_request_review:` -- which fires on review events, not on a
/// pull request opening or being pushed to, so it gates nothing -- and
/// for `pull_request` inside a comment, including the comment that
/// says it was switched off.
///
/// # `pull_request_target` is NOT accepted, and that is a change
///
/// This guard used to accept it, on the reasoning that it also runs on
/// pull requests and can be a required check. That was rust-fs-ext4#149. It runs
/// against the BASE repository with a write token and the repository's
/// secrets, and checks out the base ref by default, so a workflow
/// triggered only that way may never build the contributor's code at
/// all -- and accepting it as proof the merge is gated is permissive
/// in the worst direction for twelve library crates that take pull
/// requests from forks.
///
/// The alternative considered was to accept it conditionally, on
/// finding a checkout that names the pull request head. Both designs
/// refuse when they do not recognise the checkout, so both fail safe;
/// what settled it is that `pull_request_target` appears in ZERO of
/// the twelve repositories' workflows. The conditional branch would
/// guard a configuration that exists nowhere, and "we do not use this
/// trigger, and a test says so" is the better standing statement.
///
/// Note the narrowness of what this refuses: a workflow carrying BOTH
/// `pull_request:` and `pull_request_target:` -- the ordinary way to
/// reach secrets without giving up the gate -- is satisfied by the
/// former and never reaches this question.
fn runs_on_pull_request(wf: &Workflow) -> bool {
    wf.triggers.iter().any(|t| t == "pull_request")
}

/// Keys whose presence on a step or job means its result does not gate.
const NON_GATING_KEYS: [&str; 2] = ["if", "continue-on-error"];

fn carries_a_non_gating_key(keys: &[String]) -> bool {
    keys.iter().any(|k| NON_GATING_KEYS.contains(&k.as_str()))
}

/// Walk a workflow's steps and collect what `select` finds in each
/// `run:`.
///
/// `gating` restricts the walk to steps whose result the pull-request
/// gate actually reads: the workflow must still trigger on a pull
/// request, and neither the job nor the step may carry a key from
/// [`NON_GATING_KEYS`].
///
/// One walk, shared by both halves of the guard. BOTH HALVES ARE
/// STEP-AWARE, and that is deliberate. On the sibling `rust-fs-btrfs`
/// copy of this guard the headline assertion was left line-based while
/// only the handshake one was step-aware, so under `if: false` the
/// headline PASSED and its own failure message would have claimed the
/// pull-request gate could see an overflow when the step it names does
/// not run. Sharing the walk is what stops the two drifting apart
/// again, rather than fixing them separately twice.
fn scan_steps(workflow: &str, gating: bool, select: fn(&str) -> Vec<String>) -> Vec<String> {
    let wf = parse_workflow(workflow);
    if gating && !runs_on_pull_request(&wf) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for job in &wf.jobs {
        if gating && carries_a_non_gating_key(&job.keys) {
            continue;
        }
        for step in &job.steps {
            if gating && carries_a_non_gating_key(&step.keys) {
                continue;
            }
            out.extend(select(&step.run));
        }
    }
    out
}

/// The checking debug runs of steps that ACTUALLY GATE a pull request.
///
/// This is the function the guard below asks, and the whole of the
/// difference: [`checking_debug_runs`] finds the command, this asks
/// whether anything reads its result. Adding `if: false` to the
/// guarded step in `ci.yml`, or `continue-on-error: true`, left all
/// the guard's tests green while the gate stopped gating. See #142.
fn gating_checking_debug_runs(workflow: &str) -> Vec<String> {
    scan_steps(workflow, true, checking_debug_runs)
}

/// The guard. Reads `ci.yml` -- the workflow that gates a pull request
/// -- and refuses if nothing there compiles the overflow checks and
/// asks the build to prove it.
///
/// `ci.yml` specifically, not `release.yml`. `release.yml` already has
/// a debug run and always has; it does not run on a pull request, so
/// its presence says nothing about whether a merge was gated by it.
#[test]
fn the_pr_gate_still_tests_in_a_profile_that_can_see_an_overflow() {
    let path = manifest_dir()
        .join(".github")
        .join("workflows")
        .join("ci.yml");
    let workflow = read_or_panic(&path);

    let debug_runs = gating_checking_debug_runs(&workflow);
    assert!(
        !debug_runs.is_empty(),
        "no `cargo test` in {} runs without `--release` while setting \
         EXPECT_OVERFLOW_CHECKS=1 IN A STEP WHOSE RESULT GATES A PULL \
         REQUEST, so a defect whose only symptom is an \
         arithmetic overflow panic can merge without the PR gate ever \
         seeing it. release.yml already runs a debug suite, and that \
         does not help: it triggers on a version tag, after the change \
         has merged. If the debug step in ci.yml looked redundant beside \
         the release one, it is not -- see the comment above it.",
        path.display()
    );
}

/// Both supported host architectures must run the real fixture generator and
/// the complete Rust gate natively. The GitHub runner is already real Linux,
/// so fixture generation there must use its native kernel rather than require
/// nested virtualisation. Local macOS development still uses the matching VM.
#[test]
fn the_pr_gate_tests_x86_64_and_aarch64_natively() {
    let path = manifest_dir()
        .join(".github")
        .join("workflows")
        .join("ci.yml");
    let workflow = read_or_panic(&path);
    let documents = Yaml::load_from_str(&workflow)
        .unwrap_or_else(|e| panic!("{} is not valid YAML: {e}", path.display()));
    let document = documents
        .first()
        .unwrap_or_else(|| panic!("{} is empty", path.display()));
    let test_job = field(document, "jobs")
        .and_then(|jobs| field(jobs, "test"))
        .unwrap_or_else(|| panic!("{} has no jobs.test", path.display()));

    assert_eq!(
        field(test_job, "runs-on").and_then(Yaml::as_str),
        Some("${{ matrix.os }}"),
        "jobs.test must run each matrix row on its native runner"
    );

    let rows = field(test_job, "strategy")
        .and_then(|strategy| field(strategy, "matrix"))
        .and_then(|matrix| field(matrix, "include"))
        .and_then(Yaml::as_sequence)
        .unwrap_or_else(|| panic!("jobs.test must use an explicit strategy.matrix.include"));
    let actual: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| {
            (
                field(row, "arch").and_then(Yaml::as_str).unwrap_or(""),
                field(row, "os").and_then(Yaml::as_str).unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        actual,
        vec![("x86_64", "ubuntu-24.04"), ("aarch64", "ubuntu-24.04-arm"),],
        "the test job must cover both native standard Linux runner architectures"
    );

    let steps = field(test_job, "steps")
        .and_then(Yaml::as_sequence)
        .unwrap_or_else(|| panic!("jobs.test has no steps"));
    assert!(
        !carries_a_non_gating_key(&keys_of(test_job)),
        "jobs.test must not be conditional or allowed to fail"
    );
    for required in [
        "build-ext4-feature-images-native-linux.sh",
        "cargo clippy --locked --all-targets -- -D warnings",
        "cargo test --locked --release",
        "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib",
        "tests/scripts/*.sh",
    ] {
        assert!(
            steps.iter().any(|step| {
                field(step, "run")
                    .and_then(Yaml::as_str)
                    .is_some_and(|script| script.contains(required))
                    && !carries_a_non_gating_key(&keys_of(step))
            }),
            "jobs.test does not run `{required}` unconditionally on every native matrix row"
        );
    }

    assert!(
        steps
            .iter()
            .filter_map(|step| field(step, "run").and_then(Yaml::as_str))
            .all(|script| !script.contains("qemu-system-")),
        "a standard GitHub ARM runner must not depend on unavailable nested VM acceleration"
    );
}

/// A tag release runs on real Linux too, so it must not regain an implicit
/// `/dev/kvm` dependency after the pull-request gate has proved the native
/// fixture path. This is a shipping gate, not a slower second VM oracle.
#[test]
fn the_release_gate_generates_fixtures_natively_without_kvm() {
    let path = manifest_dir()
        .join(".github")
        .join("workflows")
        .join("release.yml");
    let workflow = read_or_panic(&path);
    let documents = Yaml::load_from_str(&workflow)
        .unwrap_or_else(|e| panic!("{} is not valid YAML: {e}", path.display()));
    let document = documents
        .first()
        .unwrap_or_else(|| panic!("{} is empty", path.display()));
    let test_job = field(document, "jobs")
        .and_then(|jobs| field(jobs, "test"))
        .unwrap_or_else(|| panic!("{} has no jobs.test", path.display()));
    let steps = field(test_job, "steps")
        .and_then(Yaml::as_sequence)
        .unwrap_or_else(|| panic!("jobs.test has no steps"));

    assert!(
        !carries_a_non_gating_key(&keys_of(test_job)),
        "release jobs.test must not be conditional or allowed to fail"
    );
    assert!(
        steps.iter().any(|step| {
            field(step, "run")
                .and_then(Yaml::as_str)
                .is_some_and(|run| {
                    run.contains("sudo bash test-disks/build-ext4-feature-images-native-linux.sh")
                })
                && !carries_a_non_gating_key(&keys_of(step))
        }),
        "release jobs.test must generate fixtures natively in an unconditional step"
    );
    assert!(
        steps
            .iter()
            .filter_map(|step| field(step, "run").and_then(Yaml::as_str))
            .all(|run| !run.contains("build-ext4-feature-images.sh")),
        "release jobs.test must not depend on a QEMU VM without guaranteed KVM"
    );
}

/// THE DISTINCTION THIS REPOSITORY NEEDS THAT A PORTED COPY WOULD MISS.
///
/// A workflow carrying a checking debug run under a name other than
/// `ci.yml` -- `release.yml`, in this repository's own case -- must not
/// satisfy the guard. Simulated here with `release.yml`'s actual step
/// shape: a plain `cargo test --locked --all-targets` with no
/// `EXPECT_OVERFLOW_CHECKS`, because that workflow was never asked to
/// carry the handshake and does not need to -- it already runs in
/// debug, unconditionally, so nothing there was ever blind. The
/// scenario worth pinning is the near miss: even a hypothetical debug
/// run in `release.yml` that DID set the handshake would not make
/// `ci.yml`'s own absence of one acceptable, because `release.yml`
/// triggers too late to gate a merge.
#[test]
fn a_checking_debug_run_that_is_not_in_ci_yml_does_not_satisfy_this_guard() {
    let release_yml_shape = "\
jobs:
  release:
    steps:
      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --all-targets
";
    // This function only ever reads ci.yml in the real guard above; this
    // test pins that the PARSER itself would still count such a line if
    // handed the wrong file, so the guard's safety is coming from WHICH
    // FILE it opens -- a fact worth being explicit about, since a future
    // edit that widened the scan to every workflow would silently stop
    // catching this repository's actual defect.
    assert_eq!(
        checking_debug_runs(release_yml_shape),
        vec!["- run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --all-targets".to_string()],
        "the parser itself would count this line -- the guard's correctness \
         depends on scanning ci.yml and ci.yml alone, not on the parser \
         refusing this shape"
    );
}

/// Whether the step's result is READ, which the line-based parser
/// above cannot see. Six conditions, each with its own test, plus a
/// control asserting the unmodified shape IS counted so the others
/// cannot pass for the wrong reason.
mod gating {
    use super::gating_checking_debug_runs as gating;

    /// The shape that does gate. Every test below is this with one
    /// thing changed, so a failure here means the fixture is wrong
    /// rather than the property.
    const GATING: &str = "\
on:
  pull_request:
    branches: [main]
jobs:
  test:
    steps:
      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib
";

    const STEP: &str = "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n";

    #[test]
    fn the_control_shape_gates() {
        assert_eq!(
            gating(GATING).len(),
            1,
            "the control must be counted, or every test below passes for the wrong reason"
        );
    }

    #[test]
    fn a_step_carrying_if_does_not_gate() {
        for condition in [
            "if: false",
            "if: ${{ false }}",
            "if: github.event_name == 'push'",
            "if: ${{ env.SOMETHING == 'yes' }}",
        ] {
            let yaml = GATING.replace(STEP, &format!("{STEP}        {condition}\n"));
            assert_ne!(yaml, GATING, "the mutation must actually apply");
            assert!(
                gating(&yaml).is_empty(),
                "a step carrying `{condition}` may or may not run, so it cannot be what \
                 makes the gate able to see an overflow. Rejected on the key's presence \
                 rather than by evaluating it -- the spellings are open-ended."
            );
        }
    }

    #[test]
    fn a_step_carrying_continue_on_error_does_not_gate() {
        let yaml = GATING.replace(STEP, &format!("{STEP}        continue-on-error: true\n"));
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert!(
            gating(&yaml).is_empty(),
            "the step runs and its failure is discarded, which is this project's own \
             named defect: a step that runs and whose result nothing reads"
        );
    }

    #[test]
    fn a_job_carrying_if_does_not_gate() {
        let yaml = GATING.replace("  test:\n", "  test:\n    if: false\n");
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert!(
            gating(&yaml).is_empty(),
            "the same reasoning one level up: a job that may not run cannot gate"
        );
    }

    #[test]
    fn a_job_carrying_continue_on_error_does_not_gate() {
        let yaml = GATING.replace("  test:\n", "  test:\n    continue-on-error: true\n");
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert!(
            gating(&yaml).is_empty(),
            "a job whose failure is discarded cannot gate, however sound its steps"
        );
    }

    /// The assumption the `ci.yml`-only scope rests on, which is a
    /// fact about the file rather than a given.
    #[test]
    fn a_workflow_that_no_longer_runs_on_pull_request_does_not_gate() {
        let yaml = GATING.replace(
            "  pull_request:\n    branches: [main]\n",
            "  push:\n    branches: [main]\n",
        );
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert!(
            gating(&yaml).is_empty(),
            "scoping the scan to ci.yml assumes ci.yml is what runs on a pull request; \
             if its triggers stop including pull_request, the step gates nothing no \
             matter how it looks"
        );
    }

    /// `pull_request_target` ALONE IS NOT A PULL-REQUEST GATE, and
    /// `runs_on_pull_request` does not count it: it compares each
    /// parsed trigger name against `pull_request` and nothing else.
    ///
    /// Such a workflow runs in the base repository's context and checks
    /// out the base ref by default, so it may never build the
    /// contributor's code (#149). Replacing `pull_request:` with
    /// `pull_request_target:` is a defeat of the gate, and this test is
    /// what kills that mutation.
    #[test]
    fn a_pull_request_target_trigger_alone_does_not_gate() {
        let yaml = GATING.replace("  pull_request:\n", "  pull_request_target:\n");
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert!(
            gating(&yaml).is_empty(),
            "pull_request_target runs against the BASE repository with a write token and \
             the repository's secrets, and checks out the base ref by default, so a \
             workflow triggered only that way may never build the contributor's code. \
             It is not proof that the merge is gated. See rust-fs-ext4#149."
        );
    }

    /// THE CONTROL THAT STOPS THE REFUSAL OVER-CORRECTING.
    ///
    /// Carrying both triggers is the ordinary way to reach secrets
    /// without giving up the gate, and such a workflow IS gated -- by
    /// its `pull_request:` key, which the refusal above must not
    /// disturb. Without this test, narrowing the comparison to
    /// `t == "pull_request" && !any(t == "pull_request_target")` would
    /// pass every other assertion in this file while refusing a
    /// perfectly gated workflow. Contributed by the branch this change
    /// supersedes; it is the arm that branch added and the reason to
    /// keep it whatever the parser looks like.
    #[test]
    fn a_workflow_carrying_both_triggers_still_gates() {
        let yaml = GATING.replace(
            "  pull_request:\n",
            "  pull_request:\n  pull_request_target:\n",
        );
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert_eq!(
            gating(&yaml).len(),
            1,
            "the workflow still triggers on pull_request, so it still gates; refusing it \
             would be the over-correction"
        );
    }

    /// A `#` inside a quoted shell value is content, not a comment.
    ///
    /// This is what `without_comment` defended, and it is the reason
    /// that helper existed: a hand-rolled scanner has to decide where
    /// a comment starts, and its own doc conceded the rule was "crude
    /// next to real YAML". The parser decides it by the grammar --
    /// inside a quoted scalar a `#` is simply a character -- so the
    /// property is asserted here rather than left to a heuristic.
    #[test]
    fn a_hash_inside_a_quoted_value_is_content_not_a_comment() {
        let yaml = GATING.replace(
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
            "      - run: echo '#1'; EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
        );
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert_eq!(
            gating(&yaml).len(),
            1,
            "the `#` is inside a quoted shell string, so the command after it is still \
             the command; treating it as a comment would refuse a correct workflow"
        );
    }

    /// A workflow the parser cannot read is a failure, never a pass.
    ///
    /// The direction matters: swallowing the error and returning an
    /// empty structure would report "no debug run gates this", which is
    /// also a failure and therefore safe -- but returning early with a
    /// pass would be the blindness this module exists to refuse.
    #[test]
    #[should_panic(expected = "not valid YAML")]
    fn a_workflow_that_does_not_parse_is_a_failure() {
        super::parse_workflow("jobs:\n  test:\n   - broken: [unclosed\n");
    }

    /// THE OTHER DIRECTION, which is the one that costs something.
    ///
    /// A step's `env:` mapping says nothing about whether its result
    /// is read, so it must NOT disqualify the step. Over-strictness
    /// here would be self-defeating: the handshake this guard looks
    /// for is itself an environment variable, and a maintainer moving
    /// it into a mapping would turn the guard red on a workflow that
    /// gates perfectly well. Pinned so a later tightening of the
    /// non-gating key list cannot quietly swallow it.
    #[test]
    fn a_step_carrying_an_env_mapping_still_gates() {
        let yaml = GATING.replace(
            STEP,
            "      - env:\n          CARGO_TERM_COLOR: always\n        run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
        );
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert_eq!(
            gating(&yaml).len(),
            1,
            "an `env:` mapping is not a condition and does not discard a result, so the \
             step still gates. Rejecting it would be over-strict in the one direction \
             that breaks a working workflow."
        );
    }

    /// A TRAILING COMMENT ON THE JOB LINE MUST NOT LOSE THE JOB.
    ///
    /// The job header is recognised by its line ending in a colon, and
    /// `  test:  # the gate` does not -- so the job's steps were never
    /// collected and the guard reported that nothing gates. Loud
    /// rather than silent, but wrong, and on a workflow that gates
    /// perfectly well. The over-strict direction is the safe one for a
    /// CONDITION; it is not safe for a comment.
    #[test]
    fn a_job_line_with_a_trailing_comment_still_gates() {
        let yaml = GATING.replace("  test:\n", "  test:  # the pull-request gate\n");
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert_eq!(
            gating(&yaml).len(),
            1,
            "a comment after the job's name says nothing about whether its result is read"
        );
    }

    /// A COMMENT IS NOT A TRIGGER. The substring search this replaced
    /// answered `true` for the comment that says the trigger was
    /// switched off, which is the most likely place the word appears
    /// on a workflow that no longer gates.
    #[test]
    fn a_commented_out_pull_request_trigger_does_not_gate() {
        for spelling in [
            "  push:\n    branches: [main]\n  # pull_request disabled for now\n",
            "  push:\n    branches: [main]\n    # was: pull_request\n",
            "  push: # replaces pull_request\n    branches: [main]\n",
        ] {
            let yaml = GATING.replace("  pull_request:\n    branches: [main]\n", spelling);
            assert_ne!(yaml, GATING, "the mutation must actually apply");
            assert!(
                yaml.contains("pull_request"),
                "precondition: the word must still be PRESENT, or this tests nothing -- \
                 the whole point is text that mentions it while not triggering on it"
            );
            assert!(
                gating(&yaml).is_empty(),
                "a workflow whose only mention of pull_request is a comment gates \
                 nothing. Spelling: {spelling:?}"
            );
        }
    }

    /// A DIFFERENT TRIGGER THAT STARTS THE SAME WAY IS A DIFFERENT
    /// TRIGGER. `pull_request_review` fires on review events, not on a
    /// pull request opening or being pushed to, so a step under it
    /// cannot be what gates the pull request.
    ///
    /// This is why the check compares whole trigger names rather than
    /// matching a prefix.
    #[test]
    fn a_similarly_named_trigger_does_not_gate() {
        for trigger in [
            "pull_request_review",
            "pull_request_review_comment",
            "pull_requests",
        ] {
            let yaml = GATING.replace("  pull_request:\n", &format!("  {trigger}:\n"));
            assert_ne!(yaml, GATING, "the mutation must actually apply");
            assert!(
                gating(&yaml).is_empty(),
                "`{trigger}` is not `pull_request`, so it must not be counted as one"
            );
        }
    }

    /// A QUOTED KEY IS THE SAME KEY. `"if": false` is valid YAML and
    /// GitHub Actions honours it exactly as `if: false`, but a raw
    /// text compare against `if` matched neither quoted spelling -- so
    /// a step that does not gate was counted as one that does.
    ///
    /// This file's manifest parser already normalises quotes, after a
    /// quoted `"overflow-checks" = false` defeated that scan. Same
    /// defect one format across.
    #[test]
    fn a_quoted_non_gating_key_still_does_not_gate() {
        for key in [
            "\"if\": false",
            "'if': false",
            "\"continue-on-error\": true",
            "'continue-on-error': true",
        ] {
            let yaml = GATING.replace(STEP, &format!("{STEP}        {key}\n"));
            assert_ne!(yaml, GATING, "the mutation must actually apply");
            assert!(
                gating(&yaml).is_empty(),
                "a step carrying `{key}` does not gate, and the quotes do not change that"
            );
        }
    }

    /// The same, one level up, where the job-level key extraction had
    /// the identical bypass.
    #[test]
    fn a_quoted_non_gating_key_on_the_job_still_does_not_gate() {
        for key in ["\"if\": false", "'continue-on-error': true"] {
            let yaml = GATING.replace("  test:\n", &format!("  test:\n    {key}\n"));
            assert_ne!(yaml, GATING, "the mutation must actually apply");
            assert!(
                gating(&yaml).is_empty(),
                "a job carrying `{key}` does not gate, quoted or not"
            );
        }
    }

    /// EVERY BLOCK-SCALAR SPELLING IS A BLOCK. `|` is not the only
    /// one: YAML's chomping and indentation indicators all open a
    /// block, and treating one as the command itself meant the block's
    /// contents were never read -- so changing `|` to `|-`, which
    /// preserves behaviour, made the guard report that nothing gated.
    #[test]
    fn a_run_block_is_read_whole_in_every_block_scalar_spelling() {
        for indicator in ["|", "|-", "|+", ">", ">-", ">+", "|2", ">2-"] {
            let yaml = format!(
                "on:\n  pull_request:\n    branches: [main]\njobs:\n  test:\n    steps:\n\
                 {}      - name: a block\n        run: {indicator}\n          set -euo pipefail\n\
                 {}          EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                "", ""
            );
            assert_eq!(
                gating(&yaml).len(),
                1,
                "`run: {indicator}` opens a block, so the command inside it must be seen"
            );
        }
    }

    /// The flow-sequence and single-scalar spellings of `on:`, which
    /// put the triggers on the same line rather than in a block.
    #[test]
    fn the_inline_trigger_spellings_are_read_too() {
        let base = GATING.replace("on:\n  pull_request:\n    branches: [main]\n", "");
        for (spelling, gates) in [
            ("on: [push, pull_request]\n", true),
            ("on: [push]\n", false),
            ("on: pull_request\n", true),
            ("on: push\n", false),
            ("on:\n  - push\n  - pull_request\n", true),
            ("on:\n  - push\n", false),
        ] {
            let yaml = format!("{spelling}{base}");
            assert_eq!(
                !gating(&yaml).is_empty(),
                gates,
                "`{spelling:?}` should {} gate",
                if gates { "" } else { "not" }
            );
        }
    }

    /// A `run: |` block is read whole, so a command inside a shell
    /// loop is visible rather than seen as fragments.
    #[test]
    fn a_run_block_is_read_whole() {
        let yaml = "\
on:
  pull_request:
    branches: [main]
jobs:
  test:
    steps:
      - name: a block
        run: |
          set -euo pipefail
          EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib
";
        assert_eq!(
            gating(yaml).len(),
            1,
            "a command inside a `run: |` block must be seen; several steps in this \
             repository's ci.yml are blocks like this one"
        );
    }

    /// The real `ci.yml` gates, read through the same function the
    /// guard uses. Distinct from the guard's own assertion: this one
    /// proves the PARSER copes with the real file's shape -- matrix
    /// strategies, `uses:`/`with:` mappings, comments between steps --
    /// rather than only with the fixtures above.
    #[test]
    fn the_real_ci_yml_still_parses_into_a_gating_step() {
        let workflow = super::read_or_panic(
            &super::manifest_dir()
                .join(".github")
                .join("workflows")
                .join("ci.yml"),
        );
        assert!(
            !gating(&workflow).is_empty(),
            "the real ci.yml must parse into at least one gating step, or the guard is \
             passing on a fixture and failing on the file it exists to read"
        );
    }
}

/// The parser is the part of this that can rot, checked against each
/// shape it has to tell apart.
mod parser {
    use super::checking_debug_runs;

    /// The trap this repository's own `ci.yml` contains: the debug
    /// command quoted verbatim in the comment explaining the step.
    /// `-r` IS `--release`, IN EVERY SPELLING CLAP ACCEPTS (#158). Each of
    /// these compiles with overflow checks off and counted as the debug
    /// run; the controls beside them are not release and still count.
    #[test]
    fn the_short_release_flag_does_not_count_in_any_spelling() {
        for line in [
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -r --all-targets",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -qr --all-targets",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -rq --all-targets",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -j4 -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -j 4 -r --lib",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --features x -r",
            "EXPECT_OVERFLOW_CHECKS=1 /usr/bin/cargo test --locked -r --lib",
            "EXPECT_OVERFLOW_CHECKS=1 cargo +1.95.0 test --locked -r --lib",
            "true&&EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -r --lib",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib 2>&1 -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --features 'a;b' -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --features \"a&&b\" -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --features a\\;b -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked '-r'",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked \"-qr\"",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked \\-r",
        ] {
            assert_eq!(
                checking_debug_runs(line),
                Vec::<String>::new(),
                "{line} builds the release profile"
            );
        }
        for line in [
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --all-targets -- -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --features r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -F r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -pr --lib",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -j r --lib",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib && rm -rf build",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib; echo -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib&&rm -rf build",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib;echo -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib|tee -r",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked -- '-r'",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib && echo 'cargo test -r'",
        ] {
            assert_eq!(
                checking_debug_runs(line).len(),
                1,
                "{line}: the r is a value or the harness's, and the run is debug"
            );
        }
    }

    #[test]
    fn a_debug_run_quoted_in_a_comment_does_not_count() {
        let yaml = "\
jobs:
  test:
    steps:
      # Measured on this branch:
      #     EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib   ->  EXIT=101
      - run: cargo test --locked --release
";
        assert_eq!(
            checking_debug_runs(yaml),
            Vec::<String>::new(),
            "a debug command quoted inside a comment is documentation, not a run"
        );
    }

    #[test]
    fn a_real_checking_debug_run_counts() {
        let yaml = "\
jobs:
  test:
    steps:
      - run: cargo test --locked --release
      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib
";
        assert_eq!(
            checking_debug_runs(yaml),
            vec!["- run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib".to_string()],
        );
    }

    /// A debug step present but never asked to check anything buys
    /// nothing over deleting it -- the whole point of the handshake.
    #[test]
    fn a_debug_run_without_the_handshake_does_not_count() {
        let yaml = "      - run: cargo test --locked --lib\n";
        assert_eq!(
            checking_debug_runs(yaml),
            Vec::<String>::new(),
            "the step runs but nothing checks the build it produced"
        );
    }

    /// A handshake on a release run proves nothing: the checks are
    /// legitimately off there.
    #[test]
    fn the_handshake_on_a_release_run_does_not_count() {
        let yaml = "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --release\n";
        assert_eq!(checking_debug_runs(yaml), Vec::<String>::new());
    }

    /// An inline trailing comment naming `--release` must not disqualify
    /// a genuine debug run.
    #[test]
    fn a_trailing_comment_naming_release_does_not_disqualify_a_debug_run() {
        let yaml =
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib  # deliberately not --release\n";
        assert_eq!(
            checking_debug_runs(yaml),
            vec!["- run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib".to_string()],
        );
    }

    /// A profile named another way still disqualifies the run.
    #[test]
    fn a_profile_flag_disqualifies_a_run_even_with_the_handshake() {
        let yaml =
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --profile release-with-debug --lib\n";
        assert_eq!(checking_debug_runs(yaml), Vec::<String>::new());
    }
}
