#![cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use rquickjs::{Context, Runtime};
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::time::{Duration, Instant};

#[test]
fn tier1_enters_for_json_global_lookup() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(1)
            .loop_threshold(1)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.stringifyKernel=function stringifyKernel(value){let result=JSON.stringify(value);return result}",
        )
        .unwrap()
    });

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    let mut result = String::new();
    while Instant::now() < deadline {
        result = context.with(|ctx| ctx.eval("stringifyKernel({answer:42})").unwrap());
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    assert_eq!(result, r#"{"answer":42}"#);
    assert!(jit.metrics().native_entries > 0, "{:?}", jit.metrics());
}
