//! The test contract, checked: no test reaches an oracle tool except
//! through the helpers that fail when it is missing, and no test announces
//! a skip.
//!
//! `chore test:unit` and `chore test:oracle` are chosen by
//! `scripts/test-targets.sh` from what each test file calls:
//! `fs_ext4_test_support::oracle_tool` / `assert_e2fsck_clean` for a tool,
//! `fixture` or a path under the fixture directory for an image. That is
//! only sound while those are the only ways in. A test that spawned
//! `e2fsck` by name would be classified as a unit test, run on the `unit`
//! CI job, and — worse — be free to return early when the tool is absent,
//! which is the silent pass the contract exists to end. So this file reads
//! every test source and refuses both shapes.
//!
//! It names the patterns it looks for without spelling them out, so that
//! this file itself stays in the unit tier.

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.rs` file under `dir`, recursively, except the support crate
/// (which is where the sanctioned helpers live) and this file (whose
/// self-test spells out the shapes it refuses).
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "support") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with(file!()) {
            out.push(path);
        }
    }
}

fn all_test_sources() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    rust_sources(&manifest_dir().join("tests"), &mut files);
    rust_sources(&manifest_dir().join("src"), &mut files);
    assert!(
        files.len() > 100,
        "found only {} sources; the scan is looking in the wrong place",
        files.len()
    );
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
            (p, text)
        })
        .collect()
}

/// The e2fsprogs programs the oracle tests use.
const TOOLS: [&str; 8] = [
    "mkfs.ext4",
    "mke2fs",
    "e2fsck",
    "fsck.ext4",
    "debugfs",
    "dumpe2fs",
    "tune2fs",
    "resize2fs",
];

/// Places in `text` where a process is spawned from a string literal that
/// names an oracle tool, or from a hard-coded sbin path.
fn direct_tool_spawns(text: &str) -> Vec<String> {
    let spawn = ["Command", "::", "new", "("].concat();
    let mut hits = Vec::new();
    for (at, _) in text.match_indices(&spawn) {
        let rest = text[at + spawn.len()..].trim_start();
        let Some(literal) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(end) = literal.find('"') else {
            continue;
        };
        let program = &literal[..end];
        let named = TOOLS.contains(&program)
            || program.contains("sbin/")
            || TOOLS.iter().any(|t| program.ends_with(&format!("/{t}")));
        if named {
            hits.push(program.to_string());
        }
    }
    // A probe of a fixed install path is how the old "is e2fsck here?"
    // skips found their tool.
    for line in text.lines() {
        let probe = ["\"/usr/", "sbin/"].concat();
        let probe_root = ["\"/", "sbin/"].concat();
        if !line.contains(&spawn)
            && (line.contains(&probe) || line.contains(&probe_root))
            && TOOLS.iter().any(|t| line.contains(t))
        {
            hits.push(line.trim().to_string());
        }
    }
    hits
}

/// Lines that print a skip notice: the signature of a test that returns
/// early and passes having checked nothing.
fn announced_skips(text: &str) -> Vec<String> {
    let print = ["eprint", "ln!("].concat();
    let lines: Vec<&str> = text.lines().collect();
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.contains(&print) {
            continue;
        }
        // The message may sit on the next line or two after rustfmt.
        let window = lines[i..lines.len().min(i + 3)].join(" ").to_lowercase();
        if window.contains("skip") {
            hits.push(format!("line {}: {}", i + 1, line.trim()));
        }
    }
    hits
}

#[test]
fn no_test_spawns_an_oracle_tool_except_through_the_support_helper() {
    let mut offenders = Vec::new();
    for (path, text) in all_test_sources() {
        for hit in direct_tool_spawns(&text) {
            offenders.push(format!("{}: {hit}", path.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "these run an oracle tool by name; use fs_ext4_test_support::oracle_tool \
         (it fails, naming `chore tools`, when the tool is missing):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn no_test_announces_a_skip() {
    let mut offenders = Vec::new();
    for (path, text) in all_test_sources() {
        for hit in announced_skips(&text) {
            offenders.push(format!("{}: {hit}", path.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "these print a skip notice. A test never skips on a missing tool or \
         fixture; fail instead (fixture / oracle_tool do):\n{}",
        offenders.join("\n")
    );
}

/// The scans find what they are for, so the two tests above cannot pass
/// by looking at nothing.
#[test]
fn the_scans_recognise_the_shapes_they_refuse() {
    let spawn = [
        "let out = Command",
        "::new(\"e2fsck\").arg(img).output();\n",
        "let dbg = Command",
        "::new(\"/usr/sbin/debugfs\");\n",
        "let ok = Command",
        "::new(tool).arg(img);\n",
        "let found = [\"/usr/",
        "sbin/e2fsck\", \"/",
        "sbin/e2fsck\"].into_iter().find(|p| exists(p));\n",
    ]
    .concat();
    let hits = direct_tool_spawns(&spawn);
    assert_eq!(hits.len(), 3, "{hits:?}");
    assert_eq!(
        hits[..2],
        ["e2fsck".to_string(), "/usr/sbin/debugfs".to_string()]
    );

    let skip = [
        "if missing {\n    eprint",
        "ln!(\n        \"SKIP: no image\"\n    );\n    return;\n}\n",
        "eprint",
        "ln!(\"note: took {ms} ms\");\n",
    ]
    .concat();
    assert_eq!(announced_skips(&skip).len(), 1);
}
