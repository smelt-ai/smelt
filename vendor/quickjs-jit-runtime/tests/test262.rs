use std::{path::Path, process::Command};

#[test]
fn interpreter() {
    let status = Command::new(env!("CARGO_BIN_EXE_jit-test262"))
        .args([
            "--mode",
            "interpreter",
            "--limit",
            "10",
            "--output",
            "../target/jit-test262/interpreter.json",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(Path::new("../target/jit-test262/interpreter.json").is_file());
}

#[test]
fn automatic() {
    let status = Command::new(env!("CARGO_BIN_EXE_jit-test262"))
        .args([
            "--mode",
            "automatic",
            "--limit",
            "10",
            "--output",
            "../target/jit-test262/automatic.json",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(Path::new("../target/jit-test262/automatic.json").is_file());
}

fn forced(mode: &str) {
    let output = format!("../target/jit-test262/{mode}.json");
    let status = Command::new(env!("CARGO_BIN_EXE_jit-test262"))
        .args([
            "--mode",
            mode,
            "--filter",
            "language/global-code/decl-func-dup.js",
            "--limit",
            "1",
            "--timeout-ms",
            "30000",
            "--output",
            &output,
        ])
        .status()
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert!(
        status.success(),
        "{mode} failed: {}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    for case in report["cases"].as_array().unwrap() {
        assert!(case["native"]["native_entries"].as_u64().unwrap() > 0);
        assert_eq!(case["native"]["unexpected_fallbacks"], 0);
        if mode == "force-tier2" {
            assert!(case["native"]["tier2_entries"].as_u64().unwrap() > 0);
        } else {
            assert!(!case["native"]["opcode_ids"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn forced_tier1_eligible() {
    forced("force-tier1");
}

#[test]
fn forced_tier2_eligible() {
    forced("force-tier2");
}
