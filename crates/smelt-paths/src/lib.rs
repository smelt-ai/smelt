//! `~/.smelt` 数据根目录的唯一解析入口。
//!
//! 存在的理由是一次真实事故：仓库里曾有 37 处各自 `dirs::home_dir().join(".smelt")`，
//! 没有任何重定向手段，于是 `cargo test` 会直接读写开发者本人的数据目录——实测能
//! 写 `app.log`/`daemon.log`、创建 `bin/`、改主库权限，甚至和正在跑的 smeltd 并发
//! 打开同一个 SQLite。测试和生产共用一份状态，迟早出事。
//!
//! 因此这里同时提供两道防线：
//! 1. `SMELT_HOME` 显式重定向，供集成测试与多实例调试使用；
//! 2. 测试二进制自动落进程专属沙箱。指向真实 `~/.smelt` 的 `SMELT_HOME` 不能把
//!    测试放出去——那正是这套沙箱要拦的事故。指向别处的路径（临时目录、父进程
//!    导出的沙箱）仍然生效。子进程不是测试二进制，必须靠 `export_to` 把沙箱传下去。

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

/// 数据根目录，默认 `~/.smelt`。
///
/// 测试二进制里，指向真实 home 的 `SMELT_HOME` 会被沙箱盖住。其它显式路径优先。
pub fn smelt_home() -> Option<PathBuf> {
    resolve(
        std::env::var_os("SMELT_HOME"),
        running_under_test().then(test_sandbox),
        dirs::home_dir(),
    )
}

/// 纯函数形式，便于用例覆盖分支而不去动进程级环境变量。
fn resolve(
    explicit: Option<OsString>,
    sandbox: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let real = home.as_ref().map(|home| home.join(".smelt"));
    let explicit = explicit
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(sandbox) = sandbox {
        if let Some(explicit) = explicit
            && real.as_ref().is_none_or(|real| &explicit != real)
        {
            return Some(explicit);
        }
        return Some(sandbox);
    }
    explicit.or(real)
}

/// 当前进程是不是 cargo 构建出来的测试二进制。
///
/// 判据是 `argv[0]` 落在 `target/<profile>/deps/` 下：cargo 的 test/bench 可执行文件
/// 固定放这里，而 `cargo run`（`target/debug/smelt`）和安装后的二进制都不在 `deps/`。
pub fn running_under_test() -> bool {
    static UNDER_TEST: OnceLock<bool> = OnceLock::new();
    *UNDER_TEST.get_or_init(|| {
        std::env::current_exe().is_ok_and(|exe| {
            exe.parent().is_some_and(|parent| {
                parent.file_name().is_some_and(|name| name == "deps")
                    && parent
                        .ancestors()
                        .any(|ancestor| ancestor.file_name().is_some_and(|name| name == "target"))
            })
        })
    })
}

/// 每个测试进程一个沙箱，互不干扰；同一进程内多次调用必须稳定，否则用例写进去的
/// 东西下一次就读不到了。
fn test_sandbox() -> PathBuf {
    static SANDBOX: OnceLock<PathBuf> = OnceLock::new();
    SANDBOX
        .get_or_init(|| {
            std::env::temp_dir()
                .join(format!("smelt-test-home-{}", std::process::id()))
                .join(".smelt")
        })
        .clone()
}

/// 把当前解析结果显式传给子进程。
///
/// 沙箱是**按进程**判定的：子进程的 `argv[0]` 不在 `deps/` 下，自己会认定"非测试"从而
/// 回落到真实 home。所以隔离必须沿进程树传播，否则第一个子进程就漏了——测试拉起的
/// smeltd 会直接跑去开发者真实的 `~/.smelt` 开库，正是这套机制要拦的事故。
pub fn export_to(command: &mut std::process::Command) {
    if let Some(root) = smelt_home() {
        command.env("SMELT_HOME", root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_override_wins_over_sandbox_and_home() {
        let resolved = resolve(
            Some(OsString::from("/tmp/explicit")),
            Some(PathBuf::from("/tmp/sandbox")),
            Some(PathBuf::from("/Users/someone")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/tmp/explicit")));
    }

    #[test]
    fn empty_override_is_ignored_so_a_blank_env_cannot_point_at_the_filesystem_root() {
        let resolved = resolve(
            Some(OsString::new()),
            None,
            Some(PathBuf::from("/Users/someone")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/Users/someone/.smelt")));
    }

    #[test]
    fn real_home_override_does_not_escape_the_sandbox() {
        let home = PathBuf::from("/Users/someone");
        let real = home.join(".smelt");
        let resolved = resolve(
            Some(real.as_os_str().to_os_string()),
            Some(PathBuf::from("/tmp/sandbox")),
            Some(home),
        );
        assert_eq!(resolved, Some(PathBuf::from("/tmp/sandbox")));
    }

    #[test]
    fn sandbox_shadows_real_home_when_no_override_is_set() {
        let resolved = resolve(
            None,
            Some(PathBuf::from("/tmp/sandbox")),
            Some(PathBuf::from("/Users/someone")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/tmp/sandbox")));
    }

    #[test]
    fn falls_back_to_real_home_only_outside_tests() {
        let resolved = resolve(None, None, Some(PathBuf::from("/Users/someone")));
        assert_eq!(resolved, Some(PathBuf::from("/Users/someone/.smelt")));
    }

    #[test]
    fn this_very_test_binary_is_detected_and_sandboxed_away_from_real_home() {
        assert!(
            running_under_test(),
            "测试二进制必须被识别，否则用例会写进开发者真实的 ~/.smelt"
        );
        let home = smelt_home().expect("沙箱路径必定存在");
        let real = dirs::home_dir().map(|home| home.join(".smelt"));
        assert_ne!(Some(home), real);
    }

    #[test]
    fn sandbox_is_stable_within_a_process() {
        assert_eq!(test_sandbox(), test_sandbox());
    }

    #[test]
    fn exported_env_lets_a_child_process_land_in_the_same_sandbox() {
        let mut command = std::process::Command::new("/bin/echo");
        export_to(&mut command);
        let exported = command
            .get_envs()
            .find(|(key, _)| *key == "SMELT_HOME")
            .and_then(|(_, value)| value)
            .map(PathBuf::from);
        assert_eq!(exported, smelt_home());
        assert_ne!(exported, dirs::home_dir().map(|home| home.join(".smelt")));
    }
}
