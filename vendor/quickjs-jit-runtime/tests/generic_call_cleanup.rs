#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Function, Object, Runtime};
use rquickjs_core::qjs;
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct TraceEvent {
    pc: u32,
    opcode: u8,
    kind: u8,
    helper_id: u8,
    reserved: u8,
}

unsafe extern "C" {
    fn JS_JitGetHelperCount(rt: *mut qjs::JSRuntime, helper: u32, count: *mut u64) -> i32;
    fn JS_JitSetExecutionTrace(
        rt: *mut qjs::JSRuntime,
        events: *mut TraceEvent,
        capacity: u32,
    ) -> i32;
    fn JS_JitGetExecutionTraceLength(
        rt: *mut qjs::JSRuntime,
        length: *mut u32,
        overflowed: *mut u32,
    ) -> i32;
}

fn exercise_cleanup(force_tier2: bool, stress_gc: bool) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(if force_tier2 {
                JitTierPolicy::Automatic
            } else {
                JitTierPolicy::BaselineOnly
            })
            .call_threshold(2)
            .force_optimized_for_test(force_tier2)
            .stress_gc(stress_gc)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        // A real host call mutates an object. It cannot have an inline or direct
        // scalar entry, and its property APIs emit no native helper trace that
        // could be mistaken for the caller's argument cleanup.
        let effect = Function::new(ctx.clone(), |value: i32, enabled: bool, object: Object<'_>| -> rquickjs::Result<i32> {
            let hits: i32 = object.get("hits")?;
            object.set("hits", hits + 1)?;
            Ok(if enabled { value } else { 0 })
        }).unwrap();
        ctx.globals().set("effect", effect).unwrap();
        ctx.eval::<(), _>("globalThis.state={hits:0};function caller(f,x,enabled,o){let result=f(x,enabled,o);return result;}").unwrap();
    });
    let invoke = || context.with(|ctx| ctx.eval::<i32, _>("caller(effect,17,true,state)").unwrap());
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics();
        assert_eq!(invoke(), 17);
        let after = jit.metrics();
        if after.native_entries - before.native_entries == 1
            && (!force_tier2 || after.tier2_entries - before.tier2_entries == 1)
            && after.pending_worker_jobs == 0
        {
            break;
        }
        jit.poll();
        assert!(
            Instant::now() < deadline,
            "generic caller never became native: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let rt = context.with(|ctx| unsafe { qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
    let mut trace = vec![TraceEvent::default(); 4096];
    assert_eq!(
        unsafe { JS_JitSetExecutionTrace(rt, trace.as_mut_ptr(), trace.len() as u32) },
        0
    );
    let before = jit.metrics();
    let helper_count = |helper| {
        let mut count = 0;
        assert_eq!(unsafe { JS_JitGetHelperCount(rt, helper, &mut count) }, 0);
        count
    };
    let calls = helper_count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
    let frees = helper_count(qjs::JSJitHelperId_JS_JIT_HELPER_FREE);
    let hits = context.with(|ctx| ctx.eval::<i32, _>("state.hits").unwrap());
    assert_eq!(invoke(), 17);
    let mut length = 0;
    let mut overflowed = 0;
    assert_eq!(
        unsafe { JS_JitGetExecutionTraceLength(rt, &mut length, &mut overflowed) },
        0
    );
    assert_eq!(
        unsafe { JS_JitSetExecutionTrace(rt, std::ptr::null_mut(), 0) },
        0
    );
    assert_eq!(overflowed, 0);
    trace.truncate(length as usize);
    let count = |helper| {
        trace
            .iter()
            .filter(|event| u32::from(event.helper_id) == helper && event.kind == 1)
            .count()
    };
    assert_eq!(
        helper_count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL) - calls,
        1,
        "generic CALL must execute"
    );
    if !force_tier2 {
        assert_eq!(count(qjs::JSJitHelperId_JS_JIT_HELPER_CALL), 1, "{trace:?}");
    }
    // Four consumed operands: the function and object own references; Int32
    // and Bool need only slot clearing, except in helper-observable stress GC.
    // The function and object are also held by globals, so their consumed
    // references are never the last ones: generated code decrements them
    // inline and only a final release (or stress GC) reaches FREE. Tier2 also
    // frees two local/return slots outside CALL consumption; those are
    // primitives or shared owners here and likewise stay inline without
    // stress GC.
    let expected_call_frees = if stress_gc { 4 } else { 0 };
    assert_eq!(
        helper_count(qjs::JSJitHelperId_JS_JIT_HELPER_FREE) - frees,
        expected_call_frees + if force_tier2 && stress_gc { 2 } else { 0 },
        "shared or primitive call operands still invoke FREE: {trace:?}"
    );
    if !force_tier2 {
        assert_eq!(
            count(qjs::JSJitHelperId_JS_JIT_HELPER_FREE),
            expected_call_frees as usize,
            "{trace:?}"
        );
        let call = trace
            .iter()
            .position(|event| {
                event.kind == 1
                    && u32::from(event.helper_id) == qjs::JSJitHelperId_JS_JIT_HELPER_CALL
            })
            .unwrap();
        assert!(
            trace
                .iter()
                .enumerate()
                .filter(|(_, event)| event.kind == 1
                    && u32::from(event.helper_id) == qjs::JSJitHelperId_JS_JIT_HELPER_FREE)
                .all(|(index, _)| index > call),
            "operands were freed before the callee used them: {trace:?}"
        );
    }
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("state.hits").unwrap()),
        hits + 1
    );
    let after = jit.metrics();
    assert_eq!(after.native_entries - before.native_entries, 1);
    if force_tier2 {
        assert_eq!(after.tier2_entries - before.tier2_entries, 1);
    }
    assert_eq!(after.native_entries, after.native_exits);

    // Accessors can reenter JavaScript during the host call. The last incoming
    // reference must remain live until the callee saves it, and the thrown
    // object must keep its exact identity while the native frame unwinds.
    let before_reentry = jit.metrics();
    assert!(context
        .with(|ctx| ctx.eval::<bool, _>(
            "globalThis.events=[];globalThis.saved=null;\
         let result=caller(effect,23,false,{\
             get hits(){events.push('get');return 4},\
             set hits(value){events.push('set:'+value);saved=this}\
         });\
         result===0 && events.join(',')==='get,set:5' && saved!==null"
        ))
        .unwrap());
    let after_reentry = jit.metrics();
    assert!(after_reentry.native_entries > before_reentry.native_entries);
    if force_tier2 {
        assert!(after_reentry.tier2_entries > before_reentry.tier2_entries);
    }
    runtime.run_gc();
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("saved.hits").unwrap()),
        4
    );
    let before_throw = jit.metrics();
    assert!(context
        .with(|ctx| ctx.eval::<bool, _>(
            "events=[];saved=null;globalThis.token={marker:91};\
         let caught=false;\
         try{caller(effect,31,true,{\
             get hits(){events.push('get');return 8},\
             set hits(value){events.push('set:'+value);saved=this;throw token}\
         });events.push('unreachable')}catch(error){caught=error===token;events.push('caught')}\
         caught && events.join(',')==='get,set:9,caught' && saved!==null && token.marker===91"
        ))
        .unwrap());
    let after_throw = jit.metrics();
    assert!(after_throw.native_entries > before_throw.native_entries);
    if force_tier2 {
        assert!(after_throw.tier2_entries > before_throw.tier2_entries);
    }
    runtime.run_gc();
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("saved.hits").unwrap()),
        8
    );
    assert_eq!(
        invoke(),
        17,
        "runtime must remain usable after exceptional cleanup"
    );
    let metrics = jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}

#[test]
fn baseline_generic_call_clears_primitive_slots_without_free_helpers() {
    exercise_cleanup(false, false);
}

#[test]
fn tier2_generic_call_clears_primitive_slots_without_free_helpers() {
    exercise_cleanup(true, false);
}

#[test]
fn stress_gc_keeps_all_generic_call_free_transitions() {
    exercise_cleanup(false, true);
    exercise_cleanup(true, true);
}

#[test]
fn tier2_generic_call_preserves_primitive_live_prefix() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let effect = Function::new(ctx.clone(), |value: i32| value + 1).unwrap();
        ctx.globals().set("prefixEffect", effect).unwrap();
        ctx.eval::<(), _>("function prefixCaller(f,x){return 1+f(x)-1}")
            .unwrap();
    });
    let invoke = || {
        context.with(|ctx| {
            let caller: Function = ctx.globals().get("prefixCaller").unwrap();
            let effect: Function = ctx.globals().get("prefixEffect").unwrap();
            caller.call::<_, i32>((effect, 41)).unwrap()
        })
    };
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics().tier2_entries;
        assert_eq!(invoke(), 42);
        jit.poll();
        if jit.metrics().tier2_entries > before {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", jit.metrics());
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..32 {
        assert_eq!(invoke(), 42);
        assert_eq!(context.with(|ctx| ctx.eval::<i32, _>("6*7").unwrap()), 42);
    }
    let metrics = jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}
