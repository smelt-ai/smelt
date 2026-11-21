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
fn tier1_enters_for_map_and_set_constructors() {
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
            "globalThis.mapKernel=function mapKernel(entries){let value=new Map(entries);return value.size};globalThis.setKernel=function setKernel(values){let value=new Set(values);return value.size}",
        )
        .unwrap()
    });

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    let mut result = (0, 0);
    while Instant::now() < deadline {
        result = context.with(|ctx| {
            let map: i32 = ctx.eval("mapKernel([['answer',42]])").unwrap();
            let set: i32 = ctx.eval("setKernel([20,22,20])").unwrap();
            (map, set)
        });
        jit.poll();
        if jit.metrics().native_entries >= 2 {
            break;
        }
    }
    assert_eq!(result, (1, 2));
    assert!(jit.metrics().native_entries >= 2, "{:?}", jit.metrics());
}

#[test]
fn tier1_constructor_keeps_borrowed_argument_slots_owned_by_the_caller() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(1)
            .loop_threshold(1)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function Pair(a,b){return {}}\n\
                 globalThis.construct=function construct(a,b){new Pair(a,b);return a+b};\n\
                 globalThis.a='20';globalThis.b='22';\n\
                 globalThis.runConstruct=function(){return construct(a,b)}",
            )
        })
        .unwrap();

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    let mut result = String::new();
    while Instant::now() < deadline {
        result = context.with(|ctx| {
            let run: rquickjs::Function<'_> = ctx.globals().get("runConstruct").unwrap();
            run.call(()).unwrap()
        });
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    assert_eq!(result, "2022");
    assert!(jit.metrics().native_entries > 0, "{:?}", jit.metrics());
}
