use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static TEST_TEMP_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn select_temp_dir(
    explicit: Option<&OsStr>,
    github_actions: bool,
    runner_temp: Option<&OsStr>,
    device_model: Option<&[u8]>,
    worktree: &Path,
    platform_temp: &Path,
) -> PathBuf {
    if let Some(path) = explicit.filter(|path| !path.is_empty()) {
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

pub fn temp_dir() -> &'static Path {
    TEST_TEMP_DIR
        .get_or_init(|| {
            let support_crate = Path::new(env!("CARGO_MANIFEST_DIR"));
            let worktree = support_crate
                .parent()
                .and_then(Path::parent)
                .expect("test support crate must live at <worktree>/tests/support");
            let model = fs::read("/proc/device-tree/model").ok();
            let selected = select_temp_dir(
                std::env::var_os("FS_EXT4_TEST_TMPDIR").as_deref(),
                std::env::var_os("GITHUB_ACTIONS").as_deref() == Some(OsStr::new("true")),
                std::env::var_os("RUNNER_TEMP").as_deref(),
                model.as_deref(),
                worktree,
                &std::env::temp_dir(),
            );
            fs::create_dir_all(&selected).unwrap_or_else(|error| {
                panic!(
                    "cannot create ext4 test scratch directory {}: {error}",
                    selected.display()
                )
            });
            selected
        })
        .as_path()
}

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
