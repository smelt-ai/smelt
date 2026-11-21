use std::{path::PathBuf, process::Command};

use serde_json::Value;

#[test]
fn runtime_pins_abi_dependencies_to_its_exact_version() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .expect("cargo metadata should run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: Value = serde_json::from_slice(&output.stdout).expect("valid metadata JSON");
    let runtime = metadata["packages"]
        .as_array()
        .and_then(|packages| {
            packages
                .iter()
                .find(|package| package["name"] == "quickjs-jit-runtime")
        })
        .expect("runtime package should be present");
    let version = runtime["version"].as_str().expect("runtime version");
    let exact = format!("={version}");

    for package in ["quickjs-jit-core", "quickjs-jit-sys"] {
        let dependency = runtime["dependencies"]
            .as_array()
            .and_then(|dependencies| {
                dependencies
                    .iter()
                    .find(|dependency| dependency["name"] == package)
            })
            .unwrap_or_else(|| panic!("missing ABI dependency {package}"));
        assert_eq!(
            dependency["req"].as_str(),
            Some(exact.as_str()),
            "{package} must be pinned exactly because its JIT ABI is patch-version coupled"
        );
    }
}
