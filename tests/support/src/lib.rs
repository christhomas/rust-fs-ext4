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
