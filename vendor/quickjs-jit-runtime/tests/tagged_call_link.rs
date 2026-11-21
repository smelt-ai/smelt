#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use rquickjs::{Context, Runtime};
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn JS_JitGetHelperCount(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        helper_id: u32,
        count: *mut u64,
    ) -> i32;
}

fn call_count(context: &Context) -> u64 {
    context.with(|ctx| {
        let rt = unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
        let mut count = 0;
        assert_eq!(
            unsafe {
                JS_JitGetHelperCount(
                    rt,
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL,
                    &mut count,
                )
            },
            0
        );
        count
    })
}

fn exercise(policy: JitTierPolicy, force_tier2: bool, miss: (&str, &str, &str, &str)) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(policy)
            .call_threshold(2)
            .loop_threshold(16)
            .force_optimized_for_test(force_tier2)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.marker={stable:true};
                 function linkedLeaf(object,value){return value+1}
                 function otherLeaf(object,value){return value+20}
                 function linkedCaller(target,object,value,n){
                   for(let i=0;i<n;i++)value=target(object,value);
                   return value;
                 }",
            )
        })
        .unwrap();
    let invoke = |target: &str, object: &str, value: &str, n: usize| -> String {
        context.with(|ctx| {
            ctx.eval::<String, _>(format!(
                "String(linkedCaller({target},{object},{value},{n}))"
            ))
            .unwrap()
        })
    };

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = call_count(&context);
        let entries = jit.metrics().native_entries;
        assert_eq!(invoke("linkedLeaf", "marker", "7", 128), "135");
        jit.poll();
        let after = jit.metrics();
        if after.pending_worker_jobs == 0
            && after.native_entries > entries
            && (!force_tier2 || after.tier2_entries > 0)
            && call_count(&context) == before
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "linked edge not ready: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let before = call_count(&context);
    assert_eq!(invoke("linkedLeaf", "marker", "7", 128), "135");
    assert_eq!(
        call_count(&context),
        before,
        "stable HeapRef edge still used generic CALL"
    );

    // The miss must resume the untouched original CALL. Each case gets a
    // fresh runtime because a Tier2 miss may deliberately demote the caller.
    // A Tier2 CALL deopt may be followed by a callee-entry representation
    // retry, so runtime transition counters are not an invocation count.
    let before = call_count(&context);
    let deopts = jit.metrics().deopts;
    assert_eq!(invoke(miss.0, miss.1, miss.2, 1), miss.3);
    assert!(
        call_count(&context) - before + jit.metrics().deopts - deopts >= 1,
        "miss did not take a compiled helper or exact CALL deopt: {miss:?}"
    );
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn baseline_target_only_heapref_link_preserves_generic_misses() {
    for miss in [
        ("otherLeaf", "marker", "7", "27"),
        ("linkedLeaf", "7", "7", "8"),
        ("linkedLeaf", "marker", "2147483647", "2147483648"),
    ] {
        exercise(JitTierPolicy::BaselineOnly, false, miss);
    }
}

#[test]
fn tier2_target_only_heapref_link_preserves_generic_misses() {
    for miss in [
        ("otherLeaf", "marker", "7", "27"),
        ("linkedLeaf", "7", "7", "8"),
        ("linkedLeaf", "marker", "2147483647", "2147483648"),
    ] {
        exercise(JitTierPolicy::Automatic, true, miss);
    }
}

fn exercise_bool_result(policy: JitTierPolicy, force_tier2: bool) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(policy)
            .call_threshold(2)
            .loop_threshold(16)
            .force_optimized_for_test(force_tier2)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.boolMarker={stable:true};
                 function linkedBool(object,enabled){return enabled}
                 function boolCaller(target,object,enabled,n){
                   let result=false;
                   for(let i=0;i<n;i++)result=target(object,enabled);
                   return result;
                 }",
            )
        })
        .unwrap();
    let invoke = |target: &str, object: &str, enabled: &str, n: usize| -> bool {
        context.with(|ctx| {
            ctx.eval::<bool, _>(format!("boolCaller({target},{object},{enabled},{n})"))
                .unwrap()
        })
    };

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = call_count(&context);
        let entries = jit.metrics().native_entries;
        assert!(invoke("linkedBool", "boolMarker", "true", 128));
        jit.poll();
        let after = jit.metrics();
        if after.pending_worker_jobs == 0
            && after.native_entries > entries
            && (!force_tier2 || after.tier2_entries > 0)
            && call_count(&context) == before
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "linked bool edge not ready: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let before = call_count(&context);
    assert!(!invoke("linkedBool", "boolMarker", "false", 128));
    assert_eq!(
        call_count(&context),
        before,
        "stable Bool result still used the generic CALL frame"
    );
    let before = call_count(&context);
    let deopts = jit.metrics().deopts;
    assert!(invoke("linkedBool", "7", "true", 1));
    assert!(
        call_count(&context) > before || jit.metrics().deopts > deopts,
        "HeapRef tag miss did not retain the generic fallback"
    );
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn baseline_target_only_heapref_link_returns_bool_without_a_call_frame() {
    exercise_bool_result(JitTierPolicy::BaselineOnly, false);
}

#[test]
fn tier2_target_only_heapref_link_returns_bool_without_a_call_frame() {
    exercise_bool_result(JitTierPolicy::Automatic, true);
}

const EFFECTFUL_SOURCE: &str = "
    globalThis.hits=0;
    globalThis.state={calls:0};
    function recordLeaf(value,enabled,state){
      state.calls=state.calls+1;
      if(enabled)return value+1;
      return value;
    }
    function bumpLeaf(value,state){state.calls++;return value+2}
    function recordCaller(target,state,value,enabled,n){
      for(let i=0;i<n;i++)value=target(value,enabled,state);
      return value;
    }
    function bumpCaller(target,state,value,n){
      for(let i=0;i<n;i++)value=target(value,state);
      return value;
    }";

struct Effectful {
    context: Context,
    jit: Jit,
    _runtime: Runtime,
}

impl Effectful {
    fn new(policy: JitTierPolicy, force_tier2: bool, stress_gc: bool) -> Self {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .tier_policy(policy)
                .call_threshold(2)
                .loop_threshold(16)
                .force_optimized_for_test(force_tier2)
                .stress_gc(stress_gc)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| ctx.eval::<(), _>(EFFECTFUL_SOURCE))
            .unwrap();
        Self {
            context,
            jit,
            _runtime: runtime,
        }
    }

    fn eval(&self, source: &str) -> String {
        self.context.with(|ctx| {
            ctx.eval::<String, _>(format!("String({source})"))
                .unwrap_or_else(|error| {
                    let exception = ctx.catch();
                    panic!("{source}: {error:?}: {exception:?}")
                })
        })
    }

    /// Warm until one complete invocation no longer reaches the generic CALL.
    fn wait_for_link(&self, expression: &str, force_tier2: bool) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            let before = call_count(&self.context);
            let entries = self.jit.metrics().native_entries;
            self.eval(expression);
            self.jit.poll();
            let after = self.jit.metrics();
            if after.pending_worker_jobs == 0
                && after.native_entries > entries
                && (!force_tier2 || after.tier2_entries > 0)
                && call_count(&self.context) == before
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "effectful linked edge not ready for {expression}: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn exercise_effectful(policy: JitTierPolicy, force_tier2: bool, stress_gc: bool) {
    let test = Effectful::new(policy, force_tier2, stress_gc);
    test.wait_for_link(
        "(state.calls=0, recordCaller(recordLeaf,state,7,true,128))",
        force_tier2,
    );
    // `state.calls++` (get_field2/post_inc) is not admitted by the Tier 2
    // body compiler, so a forced-Tier2 callee publishes no linked entry. In
    // production the callee's Baseline artifact provides it.
    if !force_tier2 {
        test.wait_for_link(
            "(state.calls=0, bumpCaller(bumpLeaf,state,7,128))",
            force_tier2,
        );
    }

    // Stable edge: exact results and effects, with no generic CALL.
    let before = call_count(&test.context);
    assert_eq!(
        test.eval("(state.calls=0, recordCaller(recordLeaf,state,7,true,128))"),
        "135"
    );
    assert_eq!(test.eval("state.calls"), "128");
    assert_eq!(test.eval("recordCaller(recordLeaf,state,7,false,64)"), "7");
    assert_eq!(test.eval("state.calls"), "192");
    if !force_tier2 {
        assert_eq!(test.eval("bumpCaller(bumpLeaf,state,1,10)"), "21");
        assert_eq!(test.eval("state.calls"), "202");
    }
    assert_eq!(
        call_count(&test.context),
        before,
        "stable effectful edge still used the generic CALL"
    );

    // Result overflow after the (buffered) store: every call increments once.
    assert_eq!(
        test.eval("(state.calls=0, recordCaller(recordLeaf,state,2147483646,true,3))"),
        "2147483649"
    );
    assert_eq!(test.eval("state.calls"), "3");
    // Counter overflow: the field becomes a double through the generic path.
    assert_eq!(
        test.eval("(state.calls=2147483647, recordCaller(recordLeaf,state,1,true,2))"),
        "3"
    );
    assert_eq!(test.eval("state.calls"), "2147483649");
    // Field tag miss (double, then string) keeps exact generic semantics.
    assert_eq!(
        test.eval("(state.calls=1.5, recordCaller(recordLeaf,state,1,true,2))"),
        "3"
    );
    assert_eq!(test.eval("state.calls"), "3.5");
    assert_eq!(
        test.eval("(state.calls='x', recordCaller(recordLeaf,state,1,true,2))"),
        "3"
    );
    assert_eq!(test.eval("state.calls"), "x11");
    // A different receiver shape misses before any write.
    assert_eq!(
        test.eval("(globalThis.other={extra:1,calls:5}, recordCaller(recordLeaf,other,1,true,2))"),
        "3"
    );
    assert_eq!(test.eval("other.calls+':'+state.calls"), "7:x11");
    // The original receiver and representation link again.
    assert_eq!(
        test.eval("(state.calls=0, recordCaller(recordLeaf,state,1,true,4))"),
        "5"
    );
    assert_eq!(test.eval("state.calls"), "4");
    // In-place shape mutations: adding a field, then read-only, then accessor.
    assert_eq!(
        test.eval("(state.added=1, recordCaller(recordLeaf,state,1,true,2))"),
        "3"
    );
    assert_eq!(test.eval("state.calls"), "6");
    // The generic put_field reports the read-only field; nothing is written.
    assert_eq!(
        test.eval(
            "(Object.defineProperty(state,'calls',{writable:false}), \
             (()=>{try{return recordCaller(recordLeaf,state,1,true,3)}\
             catch(e){return e.constructor.name+':'+state.calls}})())"
        ),
        "TypeError:6"
    );
    assert_eq!(
        test.eval(
            "(Object.defineProperty(state,'calls',{get(){return 40},set(v){hits+=v}}), \
             recordCaller(recordLeaf,state,1,true,3))"
        ),
        "4"
    );
    assert_eq!(test.eval("hits+':'+state.calls"), "123:40");
    let metrics = test.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}

#[test]
fn baseline_effectful_link_commits_field_stores_and_misses_exactly() {
    exercise_effectful(JitTierPolicy::BaselineOnly, false, false);
}

#[test]
fn tier2_effectful_link_commits_field_stores_and_misses_exactly() {
    exercise_effectful(JitTierPolicy::Automatic, true, false);
}

#[test]
fn tier2_effectful_link_is_exact_under_gc_stress() {
    exercise_effectful(JitTierPolicy::Automatic, true, true);
}

/// Production tiering. `setup` defines `caller`; `run` calls it. Warm until
/// Tier 2 is entered and one complete run makes no generic CALL. For sites
/// the Tier 2 caller cannot link (callee by global name, closure variable,
/// method receiver, or a HeapRef argument read from a global) this proves the
/// effectful callee stays frame-inlined instead of reaching the generic CALL.
fn exercise_production_site(setup: &str, run: &str, expected: &str) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(&runtime, JitConfig::builder().build().unwrap()).unwrap();
    let context = Context::full(&runtime).unwrap();
    let source = format!(
        "function leaf(v,e,s){{s.calls=s.calls+1; if(e) return v+1; return v}}
         globalThis.st={{calls:0}};
         {setup}"
    );
    context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
    let eval = |source: &str| -> String {
        context.with(|ctx| {
            ctx.eval::<String, _>(format!("String({source})"))
                .unwrap_or_else(|error| {
                    let exception = ctx.catch();
                    panic!("{source}: {error:?}: {exception:?}")
                })
        })
    };
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = call_count(&context);
        assert_eq!(eval(run), expected);
        let calls = call_count(&context) - before;
        jit.poll();
        let metrics = jit.metrics();
        if metrics.tier2_entries > 0 && metrics.pending_worker_jobs == 0 && calls == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{run}: {calls} generic CALLs per run after warmup: {metrics:?}"
        );
    }
    // Still exact after the edge is compiled: effects and results.
    assert_eq!(eval(run), expected);
    assert_eq!(eval("st.calls"), "2000");
    let metrics = jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}

#[test]
fn production_effectful_callee_by_global_name_avoids_generic_call() {
    exercise_production_site(
        "function caller(n,s){s.calls=0; let v=0; \
         for(let i=0;i<n;i++) v=leaf(v,true,s); return v+s.calls}",
        "caller(2000, st)",
        "4000",
    );
}

#[test]
fn production_effectful_callee_by_closure_avoids_generic_call() {
    exercise_production_site(
        "globalThis.caller=(function(){const f=leaf; return function(n,s){s.calls=0; \
         let v=0; for(let i=0;i<n;i++) v=f(v,true,s); return v+s.calls}})()",
        "caller(2000, st)",
        "4000",
    );
}

#[test]
fn production_effectful_callee_as_method_avoids_generic_call() {
    exercise_production_site(
        "globalThis.holder={leaf};
         function caller(n,o,s){s.calls=0; let v=0; \
         for(let i=0;i<n;i++) v=o.leaf(v,true,s); return v+s.calls}",
        "caller(2000, holder, st)",
        "4000",
    );
}

#[test]
fn production_effectful_callee_with_global_heap_argument_avoids_generic_call() {
    exercise_production_site(
        "function caller(n,f){st.calls=0; let v=0; \
         for(let i=0;i<n;i++) v=f(v,true,st); return v+st.calls}",
        "caller(2000, leaf)",
        "4000",
    );
}

#[test]
fn production_effectful_callee_from_argument_avoids_generic_call() {
    exercise_production_site(
        "function caller(n,f,s){s.calls=0; let v=0; \
         for(let i=0;i<n;i++) v=f(v,true,s); return v+s.calls}",
        "caller(2000, leaf, st)",
        "4000",
    );
}
