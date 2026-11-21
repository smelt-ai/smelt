#![cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use rquickjs::{Context, Function, Runtime};
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn JS_JitGetHelperCount(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        helper_id: u32,
        count: *mut u64,
    ) -> i32;
}

fn helper_count(rt: *mut rquickjs_core::qjs::JSRuntime, helper_id: u32) -> u64 {
    let mut count = 0;
    assert_eq!(
        unsafe { JS_JitGetHelperCount(rt, helper_id, &mut count) },
        0
    );
    count
}

fn run_until_native(source: &str, expression: &str) -> (String, rquickjs_jit::JitMetrics) {
    let runtime = Runtime::new().unwrap();
    let config = JitConfig::builder()
        .tier_policy(JitTierPolicy::BaselineOnly)
        .call_threshold(1)
        .loop_threshold(1)
        .build()
        .unwrap();
    let jit = Jit::attach(&runtime, config).unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();

    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "globalThis.__jitTestInvoke=function(){{return {expression}}}"
            ))
        })
        .unwrap();
    // Sanitizer builds can take tens of seconds to drain even this bounded,
    // stable set of function compilations when the test binary runs in
    // parallel. The function identities above remain fixed while we wait.
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    let mut result = String::new();
    while Instant::now() < deadline {
        result = context.with(|ctx| {
            let invoke: Function<'_> = ctx.globals().get("__jitTestInvoke").unwrap();
            invoke.call(()).unwrap()
        });
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    (result, jit.metrics())
}

#[test]
fn local_state_call_keeps_the_generic_native_boundary() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(2)
            .loop_threshold(1)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "function incrementIf(value,enabled){let incremented=value+1;if(enabled)return incremented;return value;}\n\
             globalThis.workloadArgument=incrementIf;\n\
             function workload(iterations,seed,target){let value=seed;for(let i=0;i<iterations;i++){value=target(value,true);}return value;}"
        )
        .unwrap();
    });
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        context.with(|ctx| {
            assert_eq!(
                ctx.eval::<i32, _>("workload(1000, 7, workloadArgument)")
                    .unwrap(),
                1007
            );
        });
        jit.poll();
        if jit.metrics().installed >= 2 && jit.metrics().pending_worker_jobs == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", jit.metrics());
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = jit.metrics();
    context.with(|ctx| {
        let rt = unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
        let calls = helper_count(rt, rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
        assert_eq!(
            ctx.eval::<i32, _>("workload(1000, 7, workloadArgument)")
                .unwrap(),
            1007
        );
        assert_eq!(
            helper_count(rt, rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL) - calls,
            1000,
            "a leaf with local state must retain the generic boundary"
        );
    });
    let after = jit.metrics();
    assert!(
        after.native_entries - before.native_entries >= 1001,
        "{after:?}"
    );
    assert_eq!(after.native_entries, after.native_exits);
    assert!(
        after.native_acquisitions - before.native_acquisitions < 100,
        "steady generic calls should reuse C entry pins: before={before:?}, after={after:?}"
    );
}

#[test]
fn production_tier1_installs_and_executes_fixed_and_generic_calls() {
    let (result, metrics) = run_until_native(
        "globalThis.g=(a,b,c,d)=>a+b+c+d;\
         globalThis.f=function f(g,a,b,c,d){let value=g(a,b,c,d);return value+0};",
        "String(f(g,5,10,12,15))",
    );
    assert_eq!(result, "42");
    assert!(metrics.installed > 0, "{metrics:?}");
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.native_entries, metrics.native_exits, "{metrics:?}");
}

#[test]
fn production_tier1_installs_and_executes_method_property_path() {
    let (result, metrics) = run_until_native(
        "globalThis.o={base:20,add(x){return this.base+x}};\
         globalThis.f=function f(o,x){o.answer=x;let value=o.add(o.answer);return value+0};",
        "String(f(o,22))",
    );
    assert_eq!(result, "42");
    assert!(metrics.installed > 0, "{metrics:?}");
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.native_entries, metrics.native_exits, "{metrics:?}");
}

#[test]
fn production_tier1_enters_for_compact_mutable_locals() {
    let (result, metrics) = run_until_native(
        "globalThis.f=function f(a){var first=a;var second=0;var third=0;var fourth=0;return first+fourth};",
        "String(f(42))",
    );
    assert_eq!(result, "42");
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.native_entries, metrics.native_exits, "{metrics:?}");
}

#[test]
fn production_tier1_enters_for_wide_control_flow() {
    let (result, metrics) = run_until_native(
        "globalThis.f=function f(a){let i=0;while(i<a){i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;i++;}return i};",
        "String(f(42))",
    );
    assert_eq!(result, "64");
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.native_entries, metrics.native_exits, "{metrics:?}");
}

#[test]
fn suspend_stops_probes_and_resume_reuses_installed_code() {
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
    context
        .with(|ctx| ctx.eval::<(), _>("function hot(a){return a+1}"))
        .unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while Instant::now() < deadline && jit.metrics().native_entries == 0 {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("hot(41)")).unwrap(),
            42
        );
        jit.poll();
    }
    let installed = jit.metrics();
    assert!(installed.native_entries > 0, "{installed:?}");

    jit.suspend().unwrap();
    assert!(jit.is_suspended());
    for _ in 0..32 {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("hot(41)")).unwrap(),
            42
        );
        jit.poll();
    }
    let suspended = jit.metrics();
    assert_eq!(suspended.native_entries, installed.native_entries);
    assert_eq!(suspended.native_exits, installed.native_exits);
    assert_eq!(suspended.hot_call_queues, installed.hot_call_queues);
    assert_eq!(suspended.hot_loop_queues, installed.hot_loop_queues);

    jit.resume().unwrap();
    assert!(!jit.is_suspended());
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("hot(41)")).unwrap(),
        42
    );
    assert!(jit.metrics().native_entries > suspended.native_entries);
}

#[test]
fn suspended_backend_detach_resets_state_for_reload() {
    let runtime = Runtime::new().unwrap();
    let config = JitConfig::builder()
        .tier_policy(JitTierPolicy::BaselineOnly)
        .build()
        .unwrap();
    let first = Jit::attach(&runtime, config.clone()).unwrap();
    first.suspend().unwrap();
    assert!(first.is_suspended());
    drop(first);

    let reloaded = Jit::attach(&runtime, config).unwrap();
    assert!(!reloaded.is_suspended());
}

#[test]
fn baseline_only_direct_call_hit_skips_call_helper_and_misses_remain_exact() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(2)
            .loop_threshold(64)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function g(a){return a+1}\n\
                 function h(a){return a+2}\n\
                 function f(fn,a){let value=fn(a);return value+0}",
            )
        })
        .unwrap();

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while Instant::now() < deadline {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("f(g,41)")).unwrap(),
            42
        );
        jit.poll();
        if jit.metrics().installed >= 3 {
            break;
        }
        std::thread::yield_now();
    }
    assert!(jit.metrics().installed >= 3, "{:?}", jit.metrics());

    let rt =
        context.with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
    assert_eq!(
        unsafe { rquickjs_core::qjs::JS_JitResetHelperCounters(rt) },
        0
    );
    for _ in 0..64 {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("f(g,41)")).unwrap(),
            42
        );
        jit.poll();
    }
    assert_eq!(
        helper_count(rt, rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL),
        0,
        "stable direct edge used generic CALL"
    );
    assert_eq!(
        helper_count(rt, rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_FREE),
        0,
        "stable scalar edge used a refcount/finalization helper"
    );
    // The argument remains an untrusted JSValue until the guarded direct edge
    // classifies it. Its ownership duplicate must therefore retain the runtime
    // helper's validation semantics; only CALL itself is eliminated on a hit.
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("f(h,40)")).unwrap(),
        42
    );
    assert_eq!(
        context.with(|ctx| ctx.eval::<f64, _>("f(g,40.5)")).unwrap(),
        41.5
    );
    assert_eq!(
        helper_count(rt, rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL),
        2,
        "target/type misses did not take the exact CALL helper edge"
    );
    // The machine-level baseline test proves the stable guarded edge does not
    // call the generic CALL helper. These target and type mutations exercise
    // the connected miss edge against the production runtime under stress GC.
}

#[test]
fn baseline_property_cache_hits_inline_and_mutation_misses_exactly() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(2)
            .loop_threshold(2)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.obj={x:0}; function bump(o,n){let i=0;while(i<n){o.x=o.x+1;i++}return o.x}",
            )
        })
        .unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while Instant::now() < deadline {
        context.with(|ctx| {
            let globals = ctx.globals();
            let bump: Function<'_> = globals.get("bump").unwrap();
            let obj: rquickjs::Object<'_> = globals.get("obj").unwrap();
            bump.call::<_, i32>((obj, 8)).unwrap();
        });
        jit.poll();
        if jit.metrics().installed >= 2 {
            break;
        }
    }
    assert!(jit.metrics().installed >= 2, "{:?}", jit.metrics());
    let rt =
        context.with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
    assert_eq!(
        unsafe { rquickjs_core::qjs::JS_JitResetHelperCounters(rt) },
        0
    );
    context
        .with(|ctx| ctx.eval::<i32, _>("bump(obj,64)"))
        .unwrap();
    assert_eq!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SHAPE_GUARD
        ),
        0,
        "the shape/generation compare is inline; hits never cross a helper"
    );
    assert_eq!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY
        ),
        0
    );
    assert_eq!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_PROPERTY
        ),
        0
    );
    let mutated = context
        .with(|ctx| ctx.eval::<i32, _>("delete obj.x; obj.y=1; obj.x=7; bump(obj,1)"))
        .unwrap();
    assert_eq!(mutated, 8);
    assert!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY
        ) > 0
            && helper_count(
                rt,
                rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_PROPERTY
            ) > 0,
        "shape mutation did not take the generic helper miss edge"
    );
}

#[test]
fn baseline_property_cache_accepts_bounded_polymorphic_shapes_under_stress_gc() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(4)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.a={x:11}; globalThis.b={pad:0,x:22}; function readx(o){return o.x}",
            )
        })
        .unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while Instant::now() < deadline {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("readx(a)")).unwrap(),
            11
        );
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("readx(b)")).unwrap(),
            22
        );
        jit.poll();
        if jit.metrics().installed >= 2 {
            break;
        }
    }
    assert!(jit.metrics().installed >= 2, "{:?}", jit.metrics());
    for _ in 0..64 {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("readx(a)")).unwrap(),
            11
        );
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("readx(b)")).unwrap(),
            22
        );
    }
}

#[test]
fn automatic_generic_call_entry_admits_the_bool_callee_to_tagged_tier2() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(1)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    // Keep the Bool callee outside the acyclic frame-inline subset: this test
    // measures admission through the tagged generic call ABI, not frame
    // inlining. The one-iteration loop preserves the original result while its
    // backedge makes that distinction explicit.
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function incrementIf(value,enabled){for(let i=0;i<1;i++){}let incremented=value+1;if(enabled)return incremented;return value;}\n\
                 globalThis.workloadArgument=incrementIf;\n\
                 function workload(iterations,seed,target){let value=seed;for(let i=0;i<iterations;i++){value=target(value,true);}return value;}"
            )
        })
        .unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 75 } else { 15 });
    loop {
        context.with(|ctx| {
            assert_eq!(
                ctx.eval::<i32, _>("workload(1000,7,workloadArgument)")
                    .unwrap(),
                1007
            )
        });
        jit.poll();
        if jit.metrics().tier2_entries >= 1000 && jit.metrics().pending_worker_jobs == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", jit.metrics());
        std::thread::sleep(Duration::from_micros(50));
    }
    let before = jit.metrics();
    context.with(|ctx| {
        assert_eq!(
            ctx.eval::<i32, _>("workload(1000,7,workloadArgument)")
                .unwrap(),
            1007
        )
    });
    let after = jit.metrics();
    assert!(after.tier2_entries - before.tier2_entries >= 1000,
        "each Bool callee still enters via the generic frame ABI: before={before:?}, after={after:?}");
    assert_eq!(after.native_entries, after.native_exits);
    assert_eq!(after.deopts, before.deopts);
}
