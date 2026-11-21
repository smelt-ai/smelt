use std::{path::Path, process::Command};

#[test]
#[ignore = "runs the pinned QuickJS scripts in a fresh Rust-host runtime"]
fn rust_host_compatible_subset_is_reported_separately() {
    let status = Command::new(env!("CARGO_BIN_EXE_jit-quickjs-suite"))
        .args([
            "--mode",
            "interpreter",
            "--limit",
            "20",
            "--output",
            "../target/jit-quickjs/interpreter.json",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(Path::new("../target/jit-quickjs/interpreter.json").is_file());
}
