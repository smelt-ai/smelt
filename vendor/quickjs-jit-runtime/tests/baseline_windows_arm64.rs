#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_os = "windows",
    target_arch = "aarch64",
    target_endian = "little"
))]

use rquickjs_core::qjs;
use rquickjs_jit::{
    bytecode::opcode,
    compiler::baseline::BaselineCompiler,
    test_support::{compile_implemented_fixture, verified_bytecode, SyntheticFrame},
};

#[test]
fn compilation_publishes_with_registered_windows_arm64_unwind_metadata() {
    let function = verified_bytecode(vec![opcode::RETURN_UNDEF], 0, 0);
    let code = compile_implemented_fixture(&BaselineCompiler::host(), &function)
        .expect("Windows ARM64 Tier 1 compilation succeeds");

    let published = code
        .publish()
        .expect("Windows ARM64 code publication and unwind registration succeeds");
    assert!(published.unwind_is_registered());
    let mut frame = SyntheticFrame::new(&[], 0, 0);
    let outcome = unsafe { frame.call(&published) };
    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
}
