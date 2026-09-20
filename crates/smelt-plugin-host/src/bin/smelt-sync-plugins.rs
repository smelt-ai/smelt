//! 把 bundled 插件集绑到某个 daemon 指纹。供 `make install` 在 exec 新 smeltd
//! 之前调用：守护启动时 mapping 必须已经存在，否则会直接放弃拉起插件。

use clap::Parser;
use smelt_plugin_host::sync_bundled_plugin_set;
use std::path::PathBuf;
use std::process;

/// 把 bundled 插件集同步到指定 smelt 根目录，并绑到 daemon 指纹。
#[derive(Debug, Parser)]
#[command(name = "smelt-sync-plugins", version, about)]
struct Cli {
    /// bundled 插件包目录。
    #[arg(long, value_name = "DIR")]
    packages: PathBuf,

    /// smelt 根目录，通常是 `~/.smelt`。
    #[arg(long, value_name = "DIR")]
    smelt_root: PathBuf,

    /// 要绑定的 smeltd 二进制路径。
    #[arg(long, value_name = "PATH")]
    daemon: PathBuf,
}

fn main() {
    let cli = Cli::parse();
    match sync_bundled_plugin_set(Some(&cli.packages), &cli.smelt_root, &cli.daemon) {
        Ok(root) => println!("{}", root.display()),
        Err(error) => {
            eprintln!("同步 bundled 插件集失败：{error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn rejects_missing_required() {
        let err =
            Cli::try_parse_from(["smelt-sync-plugins", "--packages", "/tmp/pkgs"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_all_required_paths() {
        let cli = Cli::try_parse_from([
            "smelt-sync-plugins",
            "--packages",
            "/tmp/pkgs",
            "--smelt-root",
            "/tmp/smelt",
            "--daemon",
            "/tmp/smeltd",
        ])
        .expect("合法参数应解析成功");
        assert_eq!(cli.packages, PathBuf::from("/tmp/pkgs"));
        assert_eq!(cli.smelt_root, PathBuf::from("/tmp/smelt"));
        assert_eq!(cli.daemon, PathBuf::from("/tmp/smeltd"));
    }

    #[test]
    fn rejects_unknown_args() {
        let err = Cli::try_parse_from([
            "smelt-sync-plugins",
            "--packages",
            "/tmp/pkgs",
            "--smelt-root",
            "/tmp/smelt",
            "--daemon",
            "/tmp/smeltd",
            "--nope",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }
}
