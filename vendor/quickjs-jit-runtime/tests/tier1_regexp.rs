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
fn tier1_enters_for_strings_regexp_literal() {
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
            "globalThis.regexpKernel=function regexpKernel(value){let regexp=/^[a-z]+-[0-9]+$/i;let result=regexp.test(value);return result}",
        )
        .unwrap()
    });

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    let mut result = (false, true);
    while Instant::now() < deadline {
        result = context.with(|ctx| {
            let matches: bool = ctx.eval("regexpKernel('quickjs-2026')").unwrap();
            let misses: bool = ctx.eval("regexpKernel('not a match')").unwrap();
            (matches, misses)
        });
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    assert_eq!(result, (true, false));
    assert!(jit.metrics().native_entries > 0, "{:?}", jit.metrics());
}
