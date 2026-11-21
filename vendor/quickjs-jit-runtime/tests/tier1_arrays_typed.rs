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

#[test]
fn arrays_typed_arrays_proxy_and_accessor_enter_native_with_exact_checksum() {
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
    context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.getElement = function getElement(o, k) { let v = o[k]; return v + 0; };
            globalThis.setElement = function setElement(o, k, v) { o[k] = v; return v + 0; };
            globalThis.elementKernel = function elementKernel(a, t, p, k, v) {
              setElement(a, k, v);
              setElement(t, k, getElement(a, k) + 1);
              setElement(p, k, getElement(t, k) + 1);
              return getElement(a, k) * 10000 + getElement(t, k) * 100 + getElement(p, k);
            };
            globalThis.a = [0, 0, 0];
            globalThis.t = new Float64Array(3);
            globalThis.events = [];
            globalThis.p = new Proxy({}, {
              get(o, k) { events.push('g:' + k); return o[k]; },
              set(o, k, v) { events.push('s:' + k); o[k] = v; return true; }
            });
            "#,
        )
        .unwrap()
    });

    // Keep the stable workload identity while allowing instrumented builds to
    // drain the bounded set of hot call and loop compilations.
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    let mut checksum = 0_i32;
    while Instant::now() < deadline {
        checksum = context.with(|ctx| ctx.eval("elementKernel(a,t,p,1,40)").unwrap());
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    assert_eq!(checksum, 404_142);
    let metrics = jit.metrics();
    assert!(metrics.installed > 0, "{metrics:?}");
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.native_entries, metrics.native_exits, "{metrics:?}");
    let events = context.with(|ctx| ctx.eval::<String, _>("events.slice(-2).join(',')").unwrap());
    assert_eq!(events, "s:1,g:1");
    let accessor_and_exception = context.with(|ctx| {
        ctx.eval::<String, _>(
            r#"
            (() => {
              const seen = [];
              const accessor = {
                get x() { seen.push('get'); return 41; },
                set x(v) { seen.push('set:' + v); }
              };
              setElement(accessor, 'x', 42);
              const value = getElement(accessor, 'x');
              const throwing = new Proxy({}, { get() { throw new Error('element boom'); } });
              let error;
              try { getElement(throwing, 'x'); } catch (e) { error = e.message; }
              return JSON.stringify([value, seen, error]);
            })()
            "#,
        )
        .unwrap()
    });
    assert_eq!(
        accessor_and_exception,
        r#"[41,["set:42","get"],"element boom"]"#
    );
}

#[test]
fn packed_and_typed_element_hits_skip_get_set_helpers_and_fallbacks_stay_exact() {
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
                r#"
                globalThis.packed = function packed(a, i) { a[i] = i + 3; return a[i]; };
                globalThis.i32 = function i32(a, i) { a[i] = i + 5; return a[i]; };
                globalThis.f64 = function f64(a, i) { a[i] = i + 0.5; return a[i]; };
                globalThis.readElement = function readElement(a, i) { return a[i]; };
                globalThis.writeElement = function writeElement(a, i, v) { a[i] = v; return a.length; };
                globalThis.packedArray = Array(32).fill(0);
                globalThis.i32Array = new Int32Array(32);
                globalThis.f64Array = new Float64Array(32);
                "#,
            )
        })
        .unwrap();

    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while Instant::now() < deadline {
        context.with(|ctx| {
            let globals = ctx.globals();
            let packed: Function<'_> = globals.get("packed").unwrap();
            let packed_array: rquickjs::Array<'_> = globals.get("packedArray").unwrap();
            assert_eq!(packed.call::<_, i32>((packed_array, 10)).unwrap(), 13);

            let i32: Function<'_> = globals.get("i32").unwrap();
            let i32_array: rquickjs::TypedArray<'_, i32> = globals.get("i32Array").unwrap();
            assert_eq!(i32.call::<_, i32>((i32_array, 10)).unwrap(), 15);

            let f64: Function<'_> = globals.get("f64").unwrap();
            let f64_array: rquickjs::TypedArray<'_, f64> = globals.get("f64Array").unwrap();
            assert_eq!(f64.call::<_, f64>((f64_array, 10)).unwrap(), 10.5);
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
    let entries_before = jit.metrics().native_entries;
    for _ in 0..64 {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("packed(packedArray,10)"))
                .unwrap(),
            13
        );
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("i32(i32Array,10)"))
                .unwrap(),
            15
        );
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<f64, _>("f64(f64Array,10)"))
                .unwrap(),
            10.5
        );
    }
    assert!(
        jit.metrics().native_entries > entries_before,
        "{:?}",
        jit.metrics()
    );
    assert_eq!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_ELEMENT
        ),
        0,
        "direct element loads reached the generic helper"
    );
    assert_eq!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_ELEMENT
        ),
        0,
        "direct element stores reached the generic helper"
    );

    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                r#"
                globalThis.events = [];
                globalThis.slowArray = [1, 2, 3];
                Object.defineProperty(slowArray, '1', {
                  get() { events.push('get'); return 41; },
                  set(v) { events.push('set:' + v); }
                });
                globalThis.immutableI32 = new Int32Array(
                  new ArrayBuffer(4).transferToImmutable()
                );
                "#,
            )
        })
        .unwrap();
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("readElement(slowArray,1)"))
            .unwrap(),
        41
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("writeElement(slowArray,1,9)"))
            .unwrap(),
        3
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<String, _>("events.join(',')"))
            .unwrap(),
        "get,set:9"
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("writeElement(slowArray,9,7)"))
            .unwrap(),
        10
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("i32(immutableI32,0)"))
            .unwrap(),
        0
    );
}

#[test]
fn arrays_typed_workload_keeps_native_entries_bounded_per_invocation() {
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
        .with(|ctx| ctx.eval::<(), _>(include_str!("../../benchmarks/scripts/arrays-typed.js")))
        .unwrap();

    // Sanitizer instrumentation makes each queued compilation substantially
    // slower, and this workload discovers many hot functions at once. Keep the
    // multi-install precondition while allowing the single compiler worker to
    // make progress through that queue.
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while Instant::now() < deadline {
        context.with(|ctx| {
            let workload: Function<'_> = ctx.globals().get("workload").unwrap();
            assert_eq!(
                workload
                    .call::<_, String>((2000, 0))
                    .expect("arrays-typed workload evaluates"),
                "33983000:8496750.000:2000"
            );
        });
        jit.poll();
        if jit.metrics().installed >= 2 {
            break;
        }
    }
    assert!(jit.metrics().installed >= 2, "{:?}", jit.metrics());

    let entries_before = jit.metrics().native_entries;
    for _ in 0..8 {
        context.with(|ctx| {
            let workload: Function<'_> = ctx.globals().get("workload").unwrap();
            assert_eq!(
                workload.call::<_, String>((2000, 0)).unwrap(),
                "33983000:8496750.000:2000"
            );
        });
    }
    let entries = jit.metrics().native_entries - entries_before;
    assert!(
        entries <= 32,
        "arrays-typed crossed the native boundary {entries} times for eight workload calls: {:?}",
        jit.metrics()
    );
}

fn assert_automatic_mixed_array_trial(mutation: &str) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(2)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(include_str!("../../benchmarks/scripts/arrays-typed.js"))
            .unwrap();
        ctx.eval::<(), _>(
            "globalThis.ints=new Int32Array(16);globalThis.floats=new Float64Array(16);
             for(let i=0;i<16;i++)ints[i]=i*17;",
        )
        .unwrap();
    });
    // Background compilation under sanitizers can outlast a fixed number of
    // quick interpreter calls. Allow the same bounded deadline as the array
    // workload test above, while continuing to execute and poll for Tier2.
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while Instant::now() < deadline {
        context.with(|ctx| {
            assert_eq!(
                ctx.globals()
                    .get::<_, Function>("convertAndSum")
                    .unwrap()
                    .call::<_, f64>((
                        ctx.globals().get::<_, rquickjs::Object>("ints").unwrap(),
                        ctx.globals().get::<_, rquickjs::Object>("floats").unwrap()
                    ))
                    .unwrap(),
                518.0
            );
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = jit.metrics();
    assert!(
        before.tier2_entries > 0,
        "automatic mixed numeric array trial: {before:?}"
    );
    context.with(|ctx| ctx.eval::<(), _>(mutation).unwrap());
    let after = jit.metrics();
    assert!(after.deopts > before.deopts, "{before:?} -> {after:?}");
    runtime.run_gc();
}

#[test]
fn automatic_mixed_array_trial_deopts_wrong_receiver_class() {
    assert_automatic_mixed_array_trial(
        "if(convertAndSum(new Uint8Array([1,2]),new Float64Array(2))!==1.75)
             throw Error('wrong class resumption');",
    );
}

#[test]
fn automatic_mixed_array_trial_deopts_non_numeric_element_once() {
    assert_automatic_mixed_array_trial(
        "let conversions=0;
         if(convertAndSum([1,{valueOf(){conversions++;return 2}}],new Float64Array(2))!==1.75)
             throw Error('element resumption');
         if(conversions!==1)throw Error('replayed coercion');",
    );
}

#[test]
fn automatic_mixed_array_trial_deopts_short_output_with_exact_prior_stores() {
    assert_automatic_mixed_array_trial(
        "let shortOutput=new Float64Array(1);
         if(!Number.isNaN(convertAndSum(new Int32Array([1,2]),shortOutput)))
             throw Error('short output resumption');
         if(shortOutput[0]!==0.75)throw Error('lost prior store');",
    );
}
