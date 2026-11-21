//! Production tests for effectful, frame-backed Tier 2 inlining.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Function, Object, Runtime, Value};
use rquickjs_core::qjs;
use rquickjs_jit::{Jit, JitConfig};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn JS_JitGetHelperCount(rt: *mut qjs::JSRuntime, helper: u32, count: *mut u64) -> i32;
}

struct FrameInline {
    context: Context,
    jit: Jit,
    _runtime: Runtime,
}

impl FrameInline {
    fn new(stress_gc: bool, body: &str) -> Self {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .call_threshold(2)
                .force_optimized_for_test(true)
                .stress_gc(stress_gc)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| ctx.eval::<(), _>(body)).unwrap();
        Self {
            context,
            jit,
            _runtime: runtime,
        }
    }

    fn number(&self, expression: &str) -> f64 {
        self.context
            .with(|ctx| ctx.eval::<f64, _>(expression))
            .unwrap()
    }

    fn caller_number(&self, callee_name: &str, object_name: &str, input: &str) -> f64 {
        self.context.with(|ctx| {
            let globals = ctx.globals();
            let caller: Function = globals.get("caller").unwrap();
            let callee: Function = globals.get(callee_name).unwrap();
            let object: Object = globals.get(object_name).unwrap();
            if let Ok(input) = input.parse::<i32>() {
                caller.call((callee, object, input)).unwrap()
            } else {
                let input: Value = globals.get(input).unwrap();
                caller.call((callee, object, input)).unwrap()
            }
        })
    }

    fn function_number(&self, function_name: &str, object_name: &str, input: i32) -> f64 {
        self.context.with(|ctx| {
            let globals = ctx.globals();
            let function: Function = globals.get(function_name).unwrap();
            let object: Object = globals.get(object_name).unwrap();
            function.call((object, input)).unwrap()
        })
    }

    fn count(&self, helper: u32) -> u64 {
        self.context.with(|ctx| {
            let mut count = 0;
            let runtime = unsafe { qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
            assert_eq!(
                unsafe { JS_JitGetHelperCount(runtime, helper, &mut count) },
                0
            );
            count
        })
    }

    fn wait_for_tier2(&self, expression: &str, expected: f64) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            assert_eq!(self.number(expression), expected, "{expression}");
            self.jit.poll();
            let before = self.jit.metrics();
            assert_eq!(self.number(expression), expected, "{expression}");
            let after = self.jit.metrics();
            if after.tier2_entries == before.tier2_entries + 1 && after.pending_worker_jobs == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Tier2 was not installed: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Like `wait_for_tier2`, but also waits until the Tier2 body makes no
    /// generic CALL: a slow host can enter a first caller artifact compiled
    /// before its callee's edge was ready.
    fn wait_for_linked_tier2(&self, expression: &str, expected: f64) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            assert_eq!(self.number(expression), expected, "{expression}");
            self.jit.poll();
            let before = self.jit.metrics();
            let calls = self.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            assert_eq!(self.number(expression), expected, "{expression}");
            let after = self.jit.metrics();
            if after.tier2_entries == before.tier2_entries + 1
                && self.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL) == calls
                && after.pending_worker_jobs == 0
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "linked Tier2 was not installed: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_caller_tier2(
        &self,
        callee_name: &str,
        object_name: &str,
        input: &str,
        expected: f64,
    ) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            assert_eq!(
                self.caller_number(callee_name, object_name, input),
                expected
            );
            self.jit.poll();
            let before = self.jit.metrics();
            let calls = self.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            assert_eq!(
                self.caller_number(callee_name, object_name, input),
                expected
            );
            let after = self.jit.metrics();
            let calls_after = self.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            if after.tier2_entries == before.tier2_entries + 1
                && calls_after == calls
                && after.pending_worker_jobs == 0
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Tier2 caller was not installed: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_function_artifact(
        &self,
        function_name: &str,
        object_name: &str,
        input: i32,
        expected: f64,
    ) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            self.jit.poll();
            let before = self.jit.metrics();
            assert_eq!(
                self.function_number(function_name, object_name, input),
                expected
            );
            let after = self.jit.metrics();
            if after.native_entries == before.native_entries + 1 && after.pending_worker_jobs == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "function artifact was not installed: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_compiled_artifact(&self, expression: &str, expected: f64) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        let installed = self.jit.metrics().installed;
        loop {
            assert_eq!(self.number(expression), expected, "{expression}");
            self.jit.poll();
            let metrics = self.jit.metrics();
            if metrics.installed > installed && metrics.pending_worker_jobs == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "compiled artifact was not installed: {metrics:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_native_nested_plain_caller(&self) {
        let expression = "caller(nestedOuter,plain,7)";
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            self.jit.poll();
            let before = self.jit.metrics();
            let calls = self.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            assert_eq!(self.number(expression), 14.0, "{expression}");
            let after = self.jit.metrics();
            let calls_after = self.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            if after.tier2_entries == before.tier2_entries + 1
                && calls_after == calls
                && after.pending_worker_jobs == 0
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "nested caller never reached stable native Tier2: {before:?} -> {after:?}; \
                 CALL helper count {calls} -> {calls_after}"
            );
            std::thread::yield_now();
        }
    }

    fn assert_tier2(&self, expression: &str, expected: f64) {
        let before = self.jit.metrics();
        assert_eq!(self.number(expression), expected, "{expression}");
        let after = self.jit.metrics();
        assert_eq!(
            after.tier2_entries,
            before.tier2_entries + 1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(after.native_entries, after.native_exits);
    }

    fn assert_caller_tier2(
        &self,
        callee_name: &str,
        object_name: &str,
        input: &str,
        expected: f64,
    ) {
        let before = self.jit.metrics();
        assert_eq!(
            self.caller_number(callee_name, object_name, input),
            expected
        );
        let after = self.jit.metrics();
        assert_eq!(
            after.tier2_entries,
            before.tier2_entries + 1,
            "{before:?} -> {after:?}"
        );
        assert_eq!(after.native_entries, after.native_exits);
    }
}

const SETTER_SOURCE: &str = r#"
globalThis.hits=0;
globalThis.state={};
globalThis.plain={value:0};
Object.defineProperty(state,'value',{configurable:true,set(v){hits++;this.saved=v}});
function effect(o,n){o.value=n;return n+1}
function branchEffect(o,n){let x=n+1;if(n>0){o.value=x;return x+1}o.value=n;return x-1}
function nestedInner(o,n){o.value=n;return n+1}
state.inner=nestedInner;
plain.inner=nestedInner;
function nestedOuter(o,n){let x=o.inner(o,n);return x+1}
function nestedReplacement(o,n){o.saved=8000+n;return n+100}
function replacement(o,n){o.saved=9000+n;return n+100}
function caller(f,o,n){return 5+f(o,n)}
"#;

fn post_unary_fixture(stress: bool, operator: &str) -> FrameInline {
    FrameInline::new(
        stress,
        &format!(
            r#"
            globalThis.hits=0;
            globalThis.conversions=0;
            globalThis.conversionInput={{valueOf(){{conversions++;return 41}}}};
            globalThis.plain={{value:0,after:0}};
            globalThis.state={{after:0}};
            Object.defineProperty(state,'value',{{set(v){{hits++;this.saved=v}}}});
            function effect(o,n){{o.value=1;let x=n;let old=x{operator};o.after=x;return old}}
            function caller(f,o,n){{return 5+f(o,n)}}
            "#
        ),
    )
}

#[test]
fn post_increment_and_decrement_keep_old_result_and_commit_new_local_natively() {
    for stress in [false, true] {
        for (operator, updated) in [("++", 42.0), ("--", 40.0)] {
            let test = post_unary_fixture(stress, operator);
            test.wait_for_function_artifact("effect", "plain", 7, 7.0);
            test.wait_for_caller_tier2("effect", "plain", "7", 12.0);
            let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            let before = test.jit.metrics();
            test.assert_caller_tier2("effect", "state", "41", 46.0);
            let after = test.jit.metrics();
            assert_eq!(test.number("hits"), 1.0);
            assert_eq!(test.number("state.after"), updated);
            assert_eq!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), calls);
            assert_eq!(
                after.deopts, before.deopts,
                "{operator}: {before:?} -> {after:?}"
            );
            assert_eq!(after.native_fallbacks, before.native_fallbacks);
            assert_eq!(after.native_retries, before.native_retries);
        }
    }
}

#[test]
fn post_unary_overflow_and_conversion_resume_without_replaying_effects() {
    for stress in [false, true] {
        for (operator, input, result, updated) in [
            ("++", "2147483647", 2147483652.0, 2147483648.0),
            ("--", "-2147483648", -2147483643.0, -2147483649.0),
            ("++", "conversionInput", 46.0, 42.0),
            ("--", "conversionInput", 46.0, 40.0),
        ] {
            let test = post_unary_fixture(stress, operator);
            test.wait_for_function_artifact("effect", "plain", 7, 7.0);
            test.wait_for_caller_tier2("effect", "plain", "7", 12.0);
            let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            let before = test.jit.metrics();
            assert_eq!(test.caller_number("effect", "state", input), result);
            let after = test.jit.metrics();
            assert!(after.tier2_entries > before.tier2_entries);
            assert!(
                after.deopts > before.deopts,
                "{operator} {input}: {after:?}"
            );
            assert_eq!(after.native_entries, after.native_exits);
            assert_eq!(
                test.number("hits"),
                1.0,
                "replayed setter: {operator} {input}"
            );
            assert_eq!(test.number("state.after"), updated);
            assert_eq!(
                test.number("conversions"),
                if input == "conversionInput" { 1.0 } else { 0.0 }
            );
            assert_eq!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), calls);
        }
    }
}

#[test]
fn duplicated_frame_inline_result_preserves_owners_through_gc_and_deopt() {
    for stress in [false, true] {
        let test = FrameInline::new(
            stress,
            r#"
            globalThis.state={value:0,result:7};
            globalThis.shared={marker:42};
            function effect(o,n){o.value=n;return o.result}
            function caller(f,o,n){return f(o,n)||n}
        "#,
        );
        test.wait_for_tier2("effect(state,7)", 7.0);
        test.wait_for_linked_tier2("caller(effect,state,7)", 7.0);
        let warmed = test.jit.metrics();
        assert_eq!(warmed.compile_failures, 0, "stress={stress}: {warmed:?}");
        let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
        test.assert_tier2("caller(effect,state,9)", 7.0);
        test.context
            .with(|ctx| ctx.eval::<(), _>("state.result=0").unwrap());
        test.assert_tier2("caller(effect,state,0)", 0.0);
        assert_eq!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), calls);
        let before = test.jit.metrics();
        let duplicates = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_DUP);
        test.context.with(|ctx| {
            ctx.eval::<(), _>("state.result=shared").unwrap();
            let result: rquickjs::Object = ctx.eval("caller(effect,state,7)").unwrap();
            ctx.eval::<(), _>("shared=null;state.result=null").unwrap();
            ctx.run_gc();
            assert_eq!(result.get::<_, i32>("marker").unwrap(), 42);
            drop(result);
            ctx.run_gc();
        });
        let after = test.jit.metrics();
        assert_eq!(after.tier2_entries, before.tier2_entries + 1);
        assert!(
            after.deopts > before.deopts,
            "heap truthiness must resume with both owners: {after:?}"
        );
        assert_eq!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), calls);
        // Outside stress GC the second owner is taken with an inline
        // `ref_count++`; the surviving marker above is the ownership
        // evidence. Stress GC keeps the helper and its collection points.
        if stress {
            assert!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_DUP) > duplicates);
        }
        assert_eq!(after.native_entries, after.native_exits);
        assert_eq!(after.compile_failures, 0, "stress={stress}: {after:?}");
    }
}

#[test]
fn branches_and_locals_execute_inside_the_shadow_frame() {
    let test = FrameInline::new(false, SETTER_SOURCE);
    test.wait_for_function_artifact("branchEffect", "plain", 7, 9.0);
    test.wait_for_caller_tier2("branchEffect", "plain", "7", 14.0);
    test.context
        .with(|ctx| ctx.eval::<(), _>("hits=0;state.saved=0").unwrap());
    let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
    test.assert_caller_tier2("branchEffect", "state", "11", 18.0);
    assert_eq!(test.number("hits"), 1.0);
    assert_eq!(test.number("state.saved"), 12.0);
    test.assert_caller_tier2("branchEffect", "state", "-2", 3.0);
    assert_eq!(test.number("hits"), 2.0);
    assert_eq!(test.number("state.saved"), -2.0);
    assert_eq!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), calls);
}

#[test]
fn nested_call_frame_commits_once_and_propagates_throw() {
    let test = FrameInline::new(false, SETTER_SOURCE);
    test.wait_for_tier2("nestedInner(plain,7)", 8.0);
    test.wait_for_compiled_artifact("nestedOuter(plain,7)", 9.0);
    test.wait_for_native_nested_plain_caller();
    test.context
        .with(|ctx| ctx.eval::<(), _>("hits=0;state.saved=0").unwrap());

    let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
    test.assert_tier2("caller(nestedOuter,state,11)", 18.0);
    assert_eq!(test.number("hits"), 1.0);
    assert_eq!(test.number("state.saved"), 11.0);
    assert_eq!(
        test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL),
        calls,
        "both the outer and inner CALL must be expanded as native shadow frames"
    );

    test.context.with(|ctx| {
        ctx.eval::<(), _>(
            "Object.defineProperty(state,'value',{configurable:true,set(v){hits++;throw new Error('nested')}})",
        )
        .unwrap()
    });
    let before = test.jit.metrics();
    let caught = test
        .context
        .with(|ctx| {
            ctx.eval::<bool, _>(
                "(()=>{try{caller(nestedOuter,state,12);return false}catch(e){return e.message==='nested'}})()",
            )
        })
        .unwrap();
    assert!(caught);
    let after = test.jit.metrics();
    assert_eq!(after.tier2_entries, before.tier2_entries + 1);
    assert_eq!(test.number("hits"), 2.0, "nested throw replayed an effect");
    assert_eq!(after.native_entries, after.native_exits);
}

#[test]
fn nested_dependency_replacement_and_gc_retire_the_root_caller() {
    let test = FrameInline::new(true, SETTER_SOURCE);
    test.wait_for_tier2("nestedInner(plain,7)", 8.0);
    test.wait_for_compiled_artifact("nestedOuter(plain,7)", 9.0);
    test.wait_for_native_nested_plain_caller();
    test.context.with(|ctx| {
        ctx.eval::<(), _>(
            "hits=0;state.inner=nestedInner;Object.defineProperty(state,'value',{configurable:true,set(v){hits++;state.inner=nestedReplacement;this.saved=v}})",
        )
        .unwrap()
    });
    let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
    test.assert_tier2("caller(nestedOuter,state,10)", 17.0);
    assert_eq!(test.number("hits"), 1.0);
    assert_eq!(test.number("state.saved"), 10.0);
    assert_eq!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), calls);

    let before = test.jit.metrics();
    assert_eq!(test.number("caller(nestedOuter,state,12)"), 118.0);
    let after = test.jit.metrics();
    assert_eq!(after.tier2_entries, before.tier2_entries + 1);
    assert!(
        after.native_fallbacks > before.native_fallbacks,
        "nested identity replacement did not take the guarded fallback: {before:?} -> {after:?}"
    );
    assert_eq!(test.number("hits"), 1.0);
    assert_eq!(test.number("state.saved"), 8012.0);
    assert_eq!(after.native_entries, after.native_exits);
}

#[test]
fn post_setter_overflow_resumes_at_the_callee_instruction_exactly_once() {
    let test = FrameInline::new(false, SETTER_SOURCE);
    // Compile and retain the callee before the caller requests its Tier 2 body.
    test.wait_for_function_artifact("effect", "plain", 7, 8.0);
    test.wait_for_caller_tier2("effect", "state", "7", 13.0);
    test.context
        .with(|ctx| ctx.eval::<(), _>("hits=0;state.saved=0").unwrap());
    let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
    test.assert_caller_tier2("effect", "state", "2147483647", 2147483653.0);
    assert_eq!(
        test.number("hits"),
        1.0,
        "the committed setter was replayed"
    );
    assert_eq!(test.number("state.saved"), 2147483647.0);
    assert_eq!(
        test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL),
        calls,
        "frame-backed native execution fell back to the generic CALL bridge"
    );
}

#[test]
fn leave_completes_the_parent_call_without_a_generic_call() {
    let test = FrameInline::new(false, SETTER_SOURCE);
    test.wait_for_function_artifact("effect", "plain", 7, 8.0);
    test.wait_for_caller_tier2("effect", "state", "7", 13.0);
    test.context
        .with(|ctx| ctx.eval::<(), _>("hits=0;state.saved=0").unwrap());
    let calls = test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
    test.assert_caller_tier2("effect", "state", "41", 47.0);
    assert_eq!(test.number("hits"), 1.0);
    assert_eq!(test.number("state.saved"), 41.0);
    assert_eq!(test.count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), calls);
}

#[test]
fn throwing_setter_propagates_without_replaying_the_call() {
    let test = FrameInline::new(false, SETTER_SOURCE);
    test.wait_for_tier2("effect({value:0},7)", 8.0);
    test.wait_for_tier2("caller(effect,state,7)", 13.0);
    test.context.with(|ctx| {
        ctx.eval::<(), _>(
            "hits=0;Object.defineProperty(state,'value',{configurable:true,set(v){hits++;throw new Error('setter')}})",
        )
        .unwrap()
    });
    let before = test.jit.metrics();
    let caught = test
        .context
        .with(|ctx| {
            ctx.eval::<bool, _>(
                "(()=>{try{caller(effect,state,9);return false}catch(e){return e.message==='setter'}})()",
            )
        })
        .unwrap();
    assert!(caught);
    let after = test.jit.metrics();
    assert_eq!(
        after.tier2_entries,
        before.tier2_entries + 1,
        "{before:?} -> {after:?}"
    );
    assert_eq!(test.number("hits"), 1.0, "the throwing setter was replayed");
    assert_eq!(after.native_entries, after.native_exits);
}

#[test]
fn stress_gc_and_target_replacement_never_use_the_retired_callee() {
    let test = FrameInline::new(true, SETTER_SOURCE);
    test.wait_for_function_artifact("effect", "plain", 7, 8.0);
    test.wait_for_caller_tier2("effect", "state", "7", 13.0);
    test.context.with(|ctx| {
        ctx.eval::<(), _>(
            "hits=0;Object.defineProperty(state,'value',{configurable:true,set(v){hits++;globalThis.effect=replacement;this.saved=v}})",
        )
        .unwrap()
    });
    test.assert_caller_tier2("effect", "state", "10", 16.0);
    assert_eq!(test.number("hits"), 1.0);
    // The replacement happened reentrantly at an allocating helper boundary.
    // The dependency invalidation retires the caller immediately. Its next
    // invocation must execute the replacement through a safe lower tier;
    // entering the retired Tier 2 body would return 17 and hit the old setter.
    let before_fallback = test.jit.metrics();
    assert_eq!(test.caller_number("effect", "state", "12"), 117.0);
    let after_fallback = test.jit.metrics();
    assert_eq!(
        after_fallback.tier2_entries, before_fallback.tier2_entries,
        "retired caller was entered: {before_fallback:?} -> {after_fallback:?}"
    );
    assert_eq!(test.number("hits"), 1.0);
    assert_eq!(test.number("state.saved"), 9012.0);

    // Once feedback and the replacement artifact are current, Tier 2 may be
    // published again. This is fresh code, not continued use of the retired
    // effect body.
    test.wait_for_caller_tier2("effect", "state", "12", 117.0);
    test.assert_caller_tier2("effect", "state", "13", 118.0);
    assert_eq!(test.number("hits"), 1.0);
    assert_eq!(test.number("state.saved"), 9013.0);
}
