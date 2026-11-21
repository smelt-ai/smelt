use std::process::Command;

#[test]
fn native_semantics_targets_require_their_execution_features() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
        .expect("run cargo metadata");
    assert!(output.status.success(), "cargo metadata failed");

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata JSON");
    let package = metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|package| package["name"] == "quickjs-jit-runtime")
        .expect("quickjs-jit-runtime package");

    for test_name in [
        "array_feedback",
        "array_semantics",
        "call_guard_licm",
        "deopt",
        "differential",
        "frame_inline",
        "generic_call_cleanup",
        "gpui_shell_surface",
        "helpers",
        "opcodes",
        "optimized",
        "osr",
        "profitability_backoff",
        "property_cache",
        "property_specialization",
        "return_ownership",
        "semantic_heap",
        "semantics",
        "speculative_numeric",
        "tagged_call_link",
        "test262",
        "tier1_arrays_typed",
        "tier1_calls_properties",
        "tier1_constructors",
        "tier1_globals",
        "tier1_iteration",
        "tier1_regexp",
    ] {
        let target = package["targets"]
            .as_array()
            .expect("targets array")
            .iter()
            .find(|target| target["name"] == test_name)
            .unwrap_or_else(|| panic!("missing {test_name} test target"));
        let mut features = target["required-features"]
            .as_array()
            .expect("required-features array")
            .iter()
            .map(|feature| feature.as_str().expect("feature name"))
            .collect::<Vec<_>>();
        features.sort_unstable();
        assert_eq!(features, ["compiler", "test-support"], "{test_name}");
    }
}
