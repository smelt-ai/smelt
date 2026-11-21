#![cfg(feature = "test-support")]

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use rquickjs::{
    loader::{BuiltinLoader, BuiltinResolver},
    Context as JsContext, Function, Module,
};
use rquickjs_jit::JitRuntime;

fn drain_jobs(runtime: &JitRuntime) -> (usize, usize) {
    let mut completed = 0;
    let mut exceptions = 0;
    for _ in 0..256 {
        match runtime.execute_pending_job() {
            Ok(true) => completed += 1,
            Ok(false) => return (completed, exceptions),
            Err(_) => exceptions += 1,
        }
    }
    panic!("gpui-shell fixture did not quiesce within one bounded job wave");
}

/*
 * Read-only production inventory from
 * ../gpui-component/crates/shell/src/engine/quickjs on 2026-08-30.  Each
 * discovered direct rquickjs call is represented by a named fixture below;
 * test-only shell call sites use the same surfaces and need no extra adapter.
 */
const GPUI_SHELL_QUICKJS_INVENTORY: &[(&str, u32, &str)] = &[
    ("engine/quickjs/mod.rs", 1449, "runtime_creation"),
    ("engine/quickjs/mod.rs", 1450, "full_context_creation"),
    ("engine/quickjs/mod.rs", 1464, "tuple_module_loader"),
    ("engine/quickjs/mod.rs", 1487, "interrupt_handler"),
    ("engine/quickjs/scheduler.rs", 176, "scoped_job_drain"),
    ("engine/quickjs/scheduler.rs", 206, "test_job_drain"),
    (
        "engine/quickjs/scheduler.rs",
        240,
        "transactional_job_drain",
    ),
];

#[test]
fn current_gpui_shell_quickjs_surface_runs_against_jit_runtime() {
    assert_eq!(GPUI_SHELL_QUICKJS_INVENTORY.len(), 7);

    // runtime_creation + runtime_teardown
    let runtime = JitRuntime::builder().build().expect("JIT runtime");
    let runtime_dropped = Arc::new(AtomicBool::new(false));
    rquickjs_core::runtime::test_support::set_runtime_drop_probe(&runtime, {
        let runtime_dropped = Arc::clone(&runtime_dropped);
        move || runtime_dropped.store(true, Ordering::SeqCst)
    });

    // full_context_creation
    let context = JsContext::full(&runtime).expect("full shell context");

    // tuple_module_loader
    runtime.set_loader(
        BuiltinResolver::default().with_module("gpui:surface"),
        BuiltinLoader::default().with_module("gpui:surface", b"export const base = 40;".as_slice()),
    );

    // The shell applies these limits between loader and interrupt setup.
    runtime.set_memory_limit(16 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);

    // interrupt_handler
    let interrupt_polls = Arc::new(AtomicUsize::new(0));
    runtime.set_interrupt_handler({
        let interrupt_polls = Arc::clone(&interrupt_polls);
        Some(Box::new(move || {
            interrupt_polls.fetch_add(1, Ordering::SeqCst);
            false
        }))
    });

    context.with(|ctx| {
        Module::evaluate(
            ctx.clone(),
            "gpui-shell-surface",
            r#"
            import { base } from "gpui:surface";
            globalThis.__gpuiModuleValue = base;
            "#,
        )
        .expect("module declaration")
        .finish::<()>()
        .expect("module evaluation");
        ctx.eval::<(), _>(
            "Promise.resolve(__gpuiModuleValue).then(value => { globalThis.__gpuiAsyncValue = value + 2 })",
        )
        .expect("promise scheduling");
    });

    // scoped_job_drain + test_job_drain + transactional_job_drain all use
    // this exact operation outside Context::with.
    let (jobs, exceptions) = drain_jobs(&runtime);
    assert!(jobs > 0);
    assert_eq!(exceptions, 0);
    assert!(!runtime.is_job_pending());
    let value = context.with(|ctx| ctx.globals().get::<_, i32>("__gpuiAsyncValue").unwrap());
    assert_eq!(value, 42);

    drop(context);
    assert!(!runtime_dropped.load(Ordering::SeqCst));
    drop(runtime);
    assert!(runtime_dropped.load(Ordering::SeqCst));
    let _ = interrupt_polls;
}

#[test]
fn rendering_events_and_async_continuations_share_live_script_state() {
    let runtime = JitRuntime::builder().build().expect("JIT runtime");
    let context = JsContext::full(&runtime).expect("full shell context");

    context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            class CounterView {
                constructor() {
                    this.count = 0;
                    this.status = "idle";
                    this.renders = 0;
                }
                render() {
                    this.renders++;
                    return {
                        type: "label",
                        text: `count:${this.count}`,
                        status: this.status,
                        renders: this.renders,
                    };
                }
                on_click(event) {
                    this.count += event.delta;
                    this.status = "event";
                    Promise.resolve(this.count).then(value => {
                        this.status = `settled:${value}`;
                        this.count = value + 1;
                    });
                }
            }
            globalThis.__shellView = new CounterView();
            globalThis.__shellRender = () => JSON.stringify(__shellView.render());
            globalThis.__shellClick = delta => __shellView.on_click({ delta });
            "#,
        )
        .expect("install script view");

        let first: String = ctx.eval("__shellRender()").expect("first render snapshot");
        assert_eq!(
            first,
            r#"{"type":"label","text":"count:0","status":"idle","renders":1}"#
        );

        // Exercise the same stable render entry repeatedly so the production
        // runtime is free to tier it while its object state stays observable.
        for expected in 2..=96 {
            let renders: i32 = ctx
                .eval("JSON.parse(__shellRender()).renders")
                .expect("render snapshot");
            assert_eq!(renders, expected);
        }

        ctx.eval::<(), _>("__shellClick(4)")
            .expect("event dispatch");
        let before_jobs: String = ctx
            .eval("`${__shellView.count}/${__shellView.status}`")
            .expect("synchronous event state");
        assert_eq!(before_jobs, "4/event");
    });

    // gpui-shell drains after the event, outside Context::with and while the
    // event/task ownership scope is live.
    let (jobs, exceptions) = drain_jobs(&runtime);
    assert!(jobs > 0);
    assert_eq!(exceptions, 0);

    let after_jobs = context.with(|ctx| {
        ctx.eval::<String, _>("`${__shellView.count}/${__shellView.status}`")
            .expect("continued event state")
    });
    assert_eq!(after_jobs, "5/settled:4");
}

#[test]
fn deterministic_tier1_rejection_stops_after_one_snapshot_without_queueing() {
    let runtime = JitRuntime::builder().build().expect("JIT runtime");
    assert!(!runtime.jit().test_artifact_environment_initialized());
    let context = JsContext::full(&runtime).expect("full shell context");
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                // BigInt literals (`push_bigint_i32`) have no Tier 1
                // lowering, so this continuation is a deterministic policy
                // reject.
                "globalThis.__terminalHost = value => JSON.stringify({ value, kind: String(1n) });",
            )
        })
        .unwrap();

    for _ in 0..256 {
        context.with(|ctx| {
            let function: Function = ctx.globals().get("__terminalHost").unwrap();
            assert_eq!(
                function.call::<_, String>((42,)).unwrap(),
                r#"{"value":42,"kind":"1"}"#
            );
        });
        runtime.jit().poll();
    }

    let metrics = runtime.metrics();
    assert_eq!(metrics.snapshot_requests, 1, "{metrics:?}");
    assert_eq!(metrics.queued, 0, "{metrics:?}");
    assert_eq!(metrics.compile_failures, 1, "{metrics:?}");
    assert_eq!(metrics.tier1_rejections, 1, "{metrics:?}");
    assert_eq!(metrics.pending_worker_jobs, 0, "{metrics:?}");
    assert!(!runtime.jit().test_artifact_environment_initialized());
}

#[test]
fn rejected_continuation_does_not_starve_later_shell_work() {
    let runtime = JitRuntime::builder().build().expect("JIT runtime");
    let context = JsContext::full(&runtime).expect("full shell context");
    context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.__shellJobs = [];
            Promise.resolve()
                .then(() => {
                    __shellJobs.push("before-error");
                    throw new Error("event continuation failed");
                })
                .catch(error => __shellJobs.push(error.message));
            Promise.resolve().then(() => __shellJobs.push("after-error"));
            "#,
        )
        .expect("schedule independent continuations");
    });

    let (jobs, exceptions) = drain_jobs(&runtime);
    assert!(
        jobs >= 3,
        "both independent reactions and catch must execute"
    );
    assert_eq!(exceptions, 0, "the rejection is handled by script");
    assert!(!runtime.is_job_pending());
    let jobs = context.with(|ctx| {
        ctx.eval::<String, _>("__shellJobs.join(',')")
            .expect("job trace")
    });
    assert_eq!(jobs, "before-error,after-error,event continuation failed");
}

#[test]
fn reload_generation_gc_and_runtime_teardown_are_composable() {
    let runtime = JitRuntime::builder().build().expect("JIT runtime");
    let dropped = Arc::new(AtomicBool::new(false));
    rquickjs_core::runtime::test_support::set_runtime_drop_probe(&runtime, {
        let dropped = Arc::clone(&dropped);
        move || dropped.store(true, Ordering::SeqCst)
    });
    let context = JsContext::full(&runtime).expect("full shell context");

    context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.__shellGeneration = 0;
            globalThis.__shellInstall = generation => {
                const state = { generation, events: 0 };
                state.self = state;
                const callback = delta => {
                    if (generation !== globalThis.__shellGeneration) return "stale";
                    state.events += delta;
                    return `${generation}:${state.events}`;
                };
                globalThis.__shellGeneration = generation;
                globalThis.__shellActiveState = state;
                globalThis.__shellActiveCallback = callback;
                return callback;
            };
            "#,
        )
        .expect("install application host state");
        let module = Module::evaluate(
            ctx.clone(),
            "gpui-shell-generation-1",
            r#"
            globalThis.__shellOldCallback = globalThis.__shellInstall(1);
            globalThis.__shellOldState = new WeakRef(globalThis.__shellActiveState);
            Promise.resolve().then(() => globalThis.__shellActiveCallback(2));
            export const generation = 1;
            "#,
        )
        .expect("declare first application generation");
        if let Err(error) = module.finish::<()>() {
            let thrown = ctx.catch();
            panic!("evaluate first application generation: {error:?}; thrown={thrown:?}");
        }
    });
    runtime.run_gc();
    assert_eq!(drain_jobs(&runtime).1, 0);

    context.with(|ctx| {
        let first: String = ctx.eval("__shellActiveCallback(3)").expect("old event");
        assert_eq!(first, "1:5");
        Module::evaluate(
            ctx.clone(),
            "gpui-shell-generation-2",
            r#"
            globalThis.__shellReplacement = globalThis.__shellInstall(2);
            globalThis.__shellOldResult = globalThis.__shellOldCallback(100);
            delete globalThis.__shellOldCallback;
            export const generation = 2;
            "#,
        )
        .expect("declare replacement application generation")
        .finish::<()>()
        .expect("evaluate replacement application generation");
        let stale: String = ctx
            .eval("__shellOldResult")
            .expect("retired callback result");
        assert_eq!(stale, "stale");
        let replacement: String = ctx
            .eval("__shellReplacement(7)")
            .expect("replacement callback");
        assert_eq!(replacement, "2:7");
    });

    // The retired generation contains a cycle, matching stateful view models.
    // Once its callback is retired, a full collection must reclaim the cycle
    // without disturbing the live replacement generation.
    runtime.run_gc();
    let collected = context.with(|ctx| {
        ctx.eval::<bool, _>("__shellOldState.deref() === undefined")
            .expect("retired generation weak reference")
    });
    assert!(
        collected,
        "retired cyclic application state must be collectible"
    );

    drop(context);
    assert!(!dropped.load(Ordering::SeqCst));
    drop(runtime);
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn market_sort_shape_churn_has_bounded_deopts() {
    let runtime = JitRuntime::builder().build().unwrap();
    let context = JsContext::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            function compareQuotes(left, right) { return right.score - left.score; }
            globalThis.quotes = Array.from({length: 96}, (_, index) => ({index, score: index}));
            function sortQuotes() {
                quotes.reverse();
                quotes.sort(compareQuotes);
                for (let i = 0; i < quotes.length; i++) {
                    if (quotes[i].score !== 95 - i) throw new Error("incorrect sort");
                }
            }
        "#,
        )
        .unwrap();
    });
    // Keep the training shapes alive until the real comparator is installed;
    // then change only shape identity while preserving its object/value types.
    for _ in 0..128 {
        context.with(|ctx| ctx.eval::<(), _>("sortQuotes()").unwrap());
        runtime.jit().poll();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let warm = runtime.metrics();
    assert!(warm.tier2_entries > 0, "{warm:?}");
    assert_eq!(warm.native_fallbacks, 0, "{warm:?}");
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "quotes = quotes.map(q => ({extra: true, index: q.index, score: q.score}))",
        )
        .unwrap();
        for _ in 0..128 {
            ctx.eval::<(), _>("sortQuotes()").unwrap();
        }
    });
    runtime.jit().poll();
    let metrics = runtime.metrics();
    assert!(
        metrics.native_fallbacks > 0,
        "shape change must exercise deopt: {metrics:?}"
    );
    assert!(metrics.native_fallbacks <= 20, "{metrics:?}");
    assert!(metrics.optimized_demotions > 0, "{metrics:?}");
    assert_eq!(metrics.invalid_artifacts, 0, "{metrics:?}");
}
