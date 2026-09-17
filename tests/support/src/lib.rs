use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static TEST_TEMP_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Select the configured scratch root before per-process isolation is applied.
pub fn select_temp_dir(
    explicit: Option<&OsStr>,
    managed_base: Option<&OsStr>,
    github_actions: bool,
    runner_temp: Option<&OsStr>,
    device_model: Option<&[u8]>,
    worktree: &Path,
    platform_temp: &Path,
) -> PathBuf {
    if let Some(path) = explicit.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = managed_base.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    if github_actions {
        if let Some(path) = runner_temp.filter(|path| !path.is_empty()) {
            return PathBuf::from(path);
        }
    }
    if device_model
        .map(|model| String::from_utf8_lossy(model).contains("Raspberry Pi"))
        .unwrap_or(false)
    {
        return worktree.join("tmp");
    }
    platform_temp.to_path_buf()
}

/// Create a collision-resistant per-process scratch directory below `base`.
#[doc(hidden)]
pub fn create_unique_temp_dir(base: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(base)?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..1_024_u16 {
        let candidate = base.join(format!(
            "fs-ext4-tests.{}.{}.{}",
            std::process::id(),
            started,
            attempt
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "cannot allocate unique scratch directory below {}",
            base.display()
        ),
    ))
}

/// Preserve an explicit directory or isolate a process beneath a selected root.
#[doc(hidden)]
pub fn materialize_temp_dir(explicit: Option<&OsStr>, root: &Path) -> io::Result<PathBuf> {
    if explicit.filter(|path| !path.is_empty()).is_some() {
        fs::create_dir_all(root)?;
        Ok(root.to_path_buf())
    } else {
        create_unique_temp_dir(root)
    }
}

/// Return the shared scratch directory for this integration-test process.
pub fn temp_dir() -> &'static Path {
    TEST_TEMP_DIR
        .get_or_init(|| {
            let support_crate = Path::new(env!("CARGO_MANIFEST_DIR"));
            let worktree = support_crate
                .parent()
                .and_then(Path::parent)
                .expect("test support crate must live at <worktree>/tests/support");
            let model = fs::read("/proc/device-tree/model").ok();
            let explicit = std::env::var_os("FS_EXT4_TEST_TMPDIR");
            let managed_base = std::env::var_os("FS_EXT4_TEST_TMP_BASE");
            let selected_root = select_temp_dir(
                explicit.as_deref(),
                managed_base.as_deref(),
                std::env::var_os("GITHUB_ACTIONS").as_deref() == Some(OsStr::new("true")),
                std::env::var_os("RUNNER_TEMP").as_deref(),
                model.as_deref(),
                worktree,
                &std::env::temp_dir(),
            );
            let selected = materialize_temp_dir(explicit.as_deref(), &selected_root)
                .unwrap_or_else(|error| {
                    panic!(
                        "cannot create ext4 test scratch directory below {}: {error}",
                        selected_root.display()
                    )
                });
            selected
        })
        .as_path()
}

/// Format a test filename beneath the selected scratch directory.
#[doc(hidden)]
pub fn formatted_temp_path(arguments: fmt::Arguments<'_>) -> String {
    temp_dir()
        .join(arguments.to_string())
        .to_string_lossy()
        .into_owned()
}

#[macro_export]
macro_rules! temp_path {
    ($($argument:tt)*) => {
        $crate::formatted_temp_path(format_args!($($argument)*))
    };
}

/// The path of a generated fixture under `test-disks/`, or a panic that
/// says how to build it (#137).
///
/// The images are gitignored and built by `chore fixtures` (the kernel
/// populates them, inside the fs-linux-test-harness VM). THE ONLY WAY A
/// TEST REACHES A FIXTURE: a test that found its image absent used to
/// print "skip" and return, and a skipped test reads exactly like a
/// passing one, so a checkout without fixtures ran most of the suite
/// against nothing and reported green. `chore test:unit` also relies on
/// this: a test binary that never calls it needs no fixture.
#[track_caller]
pub fn fixture(manifest_dir: &str, name: &str) -> String {
    let path = format!("{manifest_dir}/test-disks/{name}");
    assert!(
        Path::new(&path).is_file(),
        "test-disks/{name} is missing: the fixtures are gitignored and generated. \
         Build them with `chore fixtures` (it boots the fs-linux-test-harness VM; \
         `chore siblings` checks the harness out) and run the tests again. \
         Tests never skip on a missing fixture."
    );
    path
}

/// The path of oracle tool `name` (`mkfs.ext4`, `mke2fs`, `e2fsck`,
/// `debugfs`, `dumpe2fs`, `tune2fs`), or a panic naming `chore tools`.
///
/// THE ONLY WAY A TEST REACHES AN ORACLE TOOL. The oracle suites used to
/// print "skip: e2fsprogs not installed" and return, which passes having
/// checked nothing. Now the tools are installed by `chore tools` and a
/// missing one fails the test that needed it.
///
/// Looks on `PATH`, then in the sbin directories a non-root `PATH` on
/// Debian leaves out, then in Homebrew's keg-only e2fsprogs on macOS.
#[track_caller]
pub fn oracle_tool(name: &str) -> String {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    for dir in [
        "/usr/sbin",
        "/sbin",
        "/usr/local/sbin",
        "/opt/homebrew/opt/e2fsprogs/sbin",
        "/opt/homebrew/opt/e2fsprogs/bin",
        "/usr/local/opt/e2fsprogs/sbin",
        "/usr/local/opt/e2fsprogs/bin",
    ] {
        dirs.push(PathBuf::from(dir));
    }
    match dirs.iter().map(|dir| dir.join(name)).find(|p| p.is_file()) {
        Some(found) => found.to_string_lossy().into_owned(),
        None => panic!(
            "oracle tool `{name}` is not installed. Install the oracle tools with \
             `chore tools` and run the tests again. Tests never skip on a missing tool."
        ),
    }
}

/// `e2fsck -fn` on `image` must exit 0, or the test fails with its report
/// (#88).
///
/// The oracle suites checked their images with this crate's own reader,
/// which cannot see a wrong checksum. `-f` forces a full check, `-n`
/// answers no to every repair, so it reports without touching the image.
/// A missing e2fsck fails (see [`oracle_tool`]).
#[track_caller]
pub fn assert_e2fsck_clean(image: &str, tag: &str) {
    let e2fsck = oracle_tool("e2fsck");
    let out = std::process::Command::new(&e2fsck)
        .args(["-fn", image])
        .output()
        .unwrap_or_else(|error| panic!("[{tag}] run {e2fsck}: {error}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "[{tag}] e2fsck -fn {image}:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
