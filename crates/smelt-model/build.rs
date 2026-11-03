//! 编进 smelt-agent 的官方 ACP 适配器，版本只在 `agent/package.json` 写一处。
//! 出厂默认命令 `bunx --bun 包@版本` 从这里取版本，`bunx` 那条路和内置的那份就不会对不上。

use std::env;
use std::fs;
use std::path::PathBuf;

const ADAPTERS: [(&str, &str); 2] = [
    (
        "CLAUDE_AGENT_ACP_VERSION",
        "@agentclientprotocol/claude-agent-acp",
    ),
    ("CODEX_ACP_VERSION", "@agentclientprotocol/codex-acp"),
];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let package_json = manifest.join("../../agent/package.json");
    println!("cargo:rerun-if-changed={}", package_json.display());

    let manifest_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&package_json).expect("读取 agent/package.json"))
            .expect("解析 agent/package.json");
    let mut out = String::new();
    for (constant, package) in ADAPTERS {
        let version = manifest_json["dependencies"][package]
            .as_str()
            .unwrap_or_else(|| panic!("agent/package.json 的 dependencies 缺少 {package}"));
        assert!(
            version.split('.').count() == 3
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())),
            "{package} 要钉死成 x.y.z，现在是 {version:?}"
        );
        out.push_str(&format!("const {constant}: &str = {version:?};\n"));
    }
    let path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("acp_adapter_versions.rs");
    fs::write(path, out).expect("写入 ACP 适配器版本");
}
