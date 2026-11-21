//! Native return ownership and interpreter-frame publication regressions.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Function, Object, Runtime};
use rquickjs_jit::{Jit, JitConfig};

fn setup(stress: bool, source: &str) -> (Runtime, Jit, Context) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .stress_gc(stress)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
    (runtime, jit, context)
}

fn warm(jit: &Jit, mut call: impl FnMut()) {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 100 } else { 20 });
    loop {
        let before = jit.metrics();
        call();
        let after = jit.metrics();
        if after.tier2_entries == before.tier2_entries + 1
            && after.deopts == before.deopts
            && after.pending_worker_jobs == 0
        {
            return;
        }
        jit.poll();
        assert!(
            std::time::Instant::now() < deadline,
            "return kernel not ready: {after:?}"
        );
        assert_eq!(after.blacklisted, 0, "return kernel blacklisted: {after:?}");
        std::thread::sleep(std::time::Duration::from_micros(100));
    }
}

#[test]
fn borrowed_argument_return_outlives_the_original_root() {
    for stress in [false, true] {
        let (_runtime, jit, context) = setup(
            stress,
            "globalThis.shared={marker:41,nested:{answer:42}};
                 function target(value){return value}",
        );
        warm(&jit, || {
            context.with(|ctx| {
                let target: Function = ctx.globals().get("target").unwrap();
                let input: Object = ctx.globals().get("shared").unwrap();
                let returned: Object = target.call((input.clone(),)).unwrap();
                assert_eq!(returned.as_value(), input.as_value());
            })
        });
        let before = jit.metrics();
        context.with(|ctx| {
            let target: Function = ctx.globals().get("target").unwrap();
            let input: Object = ctx.globals().get("shared").unwrap();
            let returned: Object = target.call((input.clone(),)).unwrap();
            assert_eq!(returned.as_value(), input.as_value());
            drop(input);
            ctx.globals().set("shared", rquickjs::Null).unwrap();
            // The native return's owner is now the only external root.
            ctx.run_gc();
            assert_eq!(returned.get::<_, i32>("marker").unwrap(), 41);
            let nested: Object = returned.get("nested").unwrap();
            assert_eq!(nested.get::<_, i32>("answer").unwrap(), 42);
            nested.set("answer", 43).unwrap();
            ctx.run_gc();
            assert_eq!(nested.get::<_, i32>("answer").unwrap(), 43);
            drop(nested);
            drop(returned);
            ctx.run_gc();
        });
        let after = jit.metrics();
        assert_eq!(
            after.tier2_entries - before.tier2_entries,
            1,
            "stress={stress}: {before:?} -> {after:?}"
        );
        assert_eq!(after.deopts, before.deopts);
        assert_eq!(after.native_entries, after.native_exits);
    }
}

#[test]
fn unsupported_borrowed_local_copy_preserves_return_ownership_on_fallback() {
    // Copying a borrowed heap argument into a different owning local remains
    // conservatively rejected/deoptimized before put_loc. That boundary is
    // distinct from returning an already-owned helper local below.
    for stress in [false, true] {
        let (_runtime, jit, context) = setup(
            stress,
            "globalThis.shared={marker:41,nested:{answer:42}};
             function target(value){let alias=value;return alias}",
        );
        for _ in 0..64 {
            context.with(|ctx| {
                let target: Function = ctx.globals().get("target").unwrap();
                let input: Object = ctx.globals().get("shared").unwrap();
                let returned: Object = target.call((input.clone(),)).unwrap();
                assert_eq!(returned.as_value(), input.as_value());
            });
            jit.poll();
        }
        context.with(|ctx| {
            let target: Function = ctx.globals().get("target").unwrap();
            let input: Object = ctx.globals().get("shared").unwrap();
            let returned: Object = target.call((input,)).unwrap();
            ctx.globals().set("shared", rquickjs::Null).unwrap();
            ctx.run_gc();
            assert_eq!(returned.get::<_, i32>("marker").unwrap(), 41);
            let nested: Object = returned.get("nested").unwrap();
            assert_eq!(nested.get::<_, i32>("answer").unwrap(), 42);
            drop(nested);
            drop(returned);
            ctx.run_gc();
        });
        assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
    }
}

#[test]
fn helper_produced_local_return_keeps_one_owner_and_never_replays_the_getter() {
    for stress in [false, true] {
        let (_runtime, jit, context) = setup(
            stress,
            "globalThis.shared=new Int32Array(2);globalThis.getterHits=0;
             function target(value){let owned=value.length;return owned}",
        );
        warm(&jit, || {
            context.with(|ctx| {
                let target: Function = ctx.globals().get("target").unwrap();
                let input: Object = ctx.globals().get("shared").unwrap();
                assert_eq!(target.call::<_, i32>((input,)).unwrap(), 2);
            })
        });
        context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "Object.defineProperty(shared,'length',{get(){getterHits++;
             return {marker:getterHits,nested:{answer:42}}}})",
                )
            })
            .unwrap();
        let before = jit.metrics();
        context.with(|ctx| {
            let target: Function = ctx.globals().get("target").unwrap();
            let input: Object = ctx.globals().get("shared").unwrap();
            let returned: Object = target.call((input,)).unwrap();
            ctx.globals().set("shared", rquickjs::Null).unwrap();
            ctx.run_gc();
            assert_eq!(returned.get::<_, i32>("marker").unwrap(), 1);
            assert_eq!(ctx.globals().get::<_, i32>("getterHits").unwrap(), 1);
            let nested: Object = returned.get("nested").unwrap();
            nested.set("answer", 43).unwrap();
            ctx.run_gc();
            assert_eq!(nested.get::<_, i32>("answer").unwrap(), 43);
            drop(nested);
            drop(returned);
            ctx.run_gc();
        });
        let after = jit.metrics();
        assert_eq!(
            after.tier2_entries - before.tier2_entries,
            1,
            "stress={stress}: {before:?} -> {after:?}"
        );
        assert_eq!(
            after.deopts, before.deopts,
            "the helper-owned local must return from native code"
        );
        assert_eq!(after.native_entries, after.native_exits);
    }
}

#[test]
fn deferred_numeric_local_return_publishes_final_mixed_signature_loop_value() {
    for stress in [false, true] {
        let (_runtime, jit, context) = setup(
            stress,
            "function target(n,seed,enabled){let value=seed;
             for(let i=0;i<n;i++){value=enabled?value+1:value-1}return value}",
        );
        let call = |n, seed, enabled, expected| {
            context.with(|ctx| {
                let target: Function = ctx.globals().get("target").unwrap();
                assert_eq!(
                    target.call::<_, i32>((n, seed, enabled)).unwrap(),
                    expected,
                    "stress={stress}, n={n}, seed={seed}, enabled={enabled}"
                );
            })
        };
        warm(&jit, || call(3, 7, true, 10));
        for (n, seed, enabled, expected) in [
            (0, 7, true, 7),
            (1, 7, true, 8),
            (65, 7, true, 72),
            (127, 7, true, 134),
            (257, 7, false, -250),
        ] {
            let before = jit.metrics();
            call(n, seed, enabled, expected);
            let after = jit.metrics();
            assert_eq!(after.tier2_entries - before.tier2_entries, 1);
            assert_eq!(
                after.deopts, before.deopts,
                "a stable return must publish its final SSA local"
            );
            assert_eq!(after.native_entries, after.native_exits);
        }
    }
}
