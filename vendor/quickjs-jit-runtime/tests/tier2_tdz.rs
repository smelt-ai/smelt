//! Tier 2 must preserve lexical TDZ semantics.
//!
//! The verifier joins a lexical binding that is uninitialized on one incoming
//! edge and initialized on another into a plain tagged cell. Tier 2 lowers
//! `get_loc_check`/`put_loc_check` as plain frame accesses, so it may only
//! admit them where every path proves the binding initialized; otherwise the
//! interpreter's ReferenceError would be silently lost.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Runtime};
use rquickjs_jit::{Jit, JitConfig};
use std::time::{Duration, Instant};

fn interpreter_result(definition: &str, probe: &str) -> String {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(definition).unwrap();
        ctx.eval::<String, _>(probe).unwrap()
    })
}

/// Warms `warm` under forced Tier 2 until the function enters Tier 2 or the
/// bounded warmup is exhausted, then evaluates `probe` and compares it with
/// the plain interpreter. Before the fix every case entered Tier 2 within a
/// few calls and the probes returned a value instead of the ReferenceError.
fn tier2_case(definition: &str, warm: &str, probe: &str) {
    let expected = interpreter_result(definition, probe);
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(definition)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    for _ in 0..300 {
        context.with(|ctx| ctx.eval::<(), _>(warm)).unwrap();
        jit.poll();
        if jit.metrics().tier2_entries > 0 || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..4 {
        let actual = context.with(|ctx| ctx.eval::<String, _>(probe)).unwrap();
        assert_eq!(
            actual,
            expected,
            "TDZ result differs from QuickJS for {definition} / {probe}: {:?}",
            jit.metrics()
        );
    }
}

const PROBE_WRAP: &str = "(function(){try{return 'value:'+String(PROBE)}catch(e){return e.constructor.name+':'+e.message}})()";

fn probe(expression: &str) -> String {
    PROBE_WRAP.replace("PROBE", expression)
}

#[test]
fn switch_fallthrough_tdz_read_throws_after_tier2_warmup() {
    tier2_case(
        "function f(k){switch(k){case 0: let y=1; case 1: return y}}",
        "f(0)",
        &probe("f(1)"),
    );
}

#[test]
fn switch_fallthrough_tdz_read_in_loop_throws_after_tier2_warmup() {
    tier2_case(
        "function f(n,k){let s=0;for(let i=0;i<n;i++){switch(k){case 0: let y=i; case 1: s=y;}}return s}",
        "f(20,0)",
        &probe("f(3,1)"),
    );
}

#[test]
fn switch_fallthrough_tdz_write_throws_after_tier2_warmup() {
    tier2_case(
        "function f(k){switch(k){case 0: let y=1; case 1: y=2; return y}}",
        "f(0)",
        &probe("f(1)"),
    );
}

#[test]
fn straight_line_tdz_read_throws_after_tier2_warmup() {
    tier2_case(
        "function f(k){if(k) return y; let y=k+1; return y}",
        "f(0)",
        &probe("f(1)"),
    );
}

#[test]
fn initialized_switch_path_still_matches_interpreter() {
    tier2_case(
        "function f(k){switch(k){case 0: let y=1; case 1: return y}}",
        "f(0)",
        &probe("f(0)"),
    );
}
