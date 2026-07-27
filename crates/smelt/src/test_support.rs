#![cfg(test)]

use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const OVERRIDE_VARS: [&str; 5] = [
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GROK_HOME",
    "COPILOT_HOME",
    "XDG_CONFIG_HOME",
];

static HOME_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct HomeEnvGuard {
    prev_home: Option<OsString>,
    prev_overrides: Vec<(&'static str, Option<OsString>)>,
}

impl HomeEnvGuard {
    fn new(home: &Path) -> Self {
        let prev_home = std::env::var_os("HOME");
        let prev_overrides = OVERRIDE_VARS
            .iter()
            .map(|var| (*var, std::env::var_os(var)))
            .collect();
        unsafe {
            std::env::set_var("HOME", home);
            for var in OVERRIDE_VARS {
                std::env::remove_var(var);
            }
        }
        Self {
            prev_home,
            prev_overrides,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.prev_home.take() {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            for (var, prev) in self.prev_overrides.drain(..) {
                match prev {
                    Some(value) => std::env::set_var(var, value),
                    None => std::env::remove_var(var),
                }
            }
        }
    }
}

fn test_smeltd_source() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SMELT_TEST_SMELTD_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/smeltd")
        .canonicalize()
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/smeltd")
        });
    fallback.is_file().then_some(fallback)
}

fn stage_smeltd(home: &Path) {
    let Some(src) = test_smeltd_source() else {
        return;
    };
    let bin_dir = home.join(".smelt").join("bin");
    if std::fs::create_dir_all(&bin_dir).is_err() {
        return;
    }
    let dst = bin_dir.join("smeltd");
    if dst.is_file() {
        return;
    }
    if std::fs::copy(&src, &dst).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&dst) {
                let mut perm = meta.permissions();
                perm.set_mode(0o755);
                let _ = std::fs::set_permissions(&dst, perm);
            }
        }
    }
}

pub(crate) fn with_home<R>(home: &Path, f: impl FnOnce() -> R) -> R {
    stage_smeltd(home);
    let _lock = HOME_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _guard = HomeEnvGuard::new(home);
    f()
}

pub(crate) fn test_artifacts_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SMELT_TEST_ARTIFACTS_ROOT") {
        return PathBuf::from(root);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".smelt-test-artifacts")
}
