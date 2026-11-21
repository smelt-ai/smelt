use std::{sync::Arc, time::Instant};

use rquickjs::{Context, Runtime};
use rquickjs_jit::{
    bytecode::{opcode, CompileSnapshot, VerifyLimits},
    code_cache::CompiledArtifact,
    compiler::mock::FakeCompiler,
    runtime::{BackgroundCompiler, CompileState, Coordinator, FunctionKey, Tier},
    JitTierPolicy,
};

fn snapshot(id: u64, generation: u64) -> rquickjs_jit::bytecode::VerifiedFunction {
    let _ = (id, generation);
    CompileSnapshot::from_untrusted_bytecode(vec![opcode::RETURN_UNDEF], 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap()
}

#[test]
fn foreground_submission_never_waits_for_blocked_compiler() {
    let (compiler, control) = FakeCompiler::new(1);
    let mut workers = BackgroundCompiler::new(Arc::new(compiler), 1, 1).unwrap();
    let mut coordinator = Coordinator::with_limits(1, 1, 4, 1024);
    let key = FunctionKey::new(7, 1);
    coordinator
        .queue(key, Tier::Baseline, snapshot(7, 1))
        .unwrap();

    let started = Instant::now();
    assert!(workers.dispatch_next(&mut coordinator).unwrap());
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
    assert_eq!(control.next_request().unwrap().key(), key);
    let usage = workers.live_usage();
    assert_eq!(usage.0, 1);
    assert!(usage.1 > 0);
    assert!(usage.2 >= usage.1);
    assert_eq!(coordinator.drain_completions().drained(), 0);

    control.complete(CompiledArtifact::fake(Tier::Baseline));
    let deadline = Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while workers.live_usage().0 != 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(workers.live_usage().0, 0);
    workers.shutdown(&mut coordinator);
    assert_eq!(workers.live_usage(), (0, 0, 0));
    assert_eq!(
        coordinator.state(key),
        CompileState::Installed(Tier::Baseline)
    );
}

#[test]
fn configured_workers_receive_compilations_concurrently() {
    let (compiler, control) = FakeCompiler::new(2);
    let mut workers = BackgroundCompiler::new(Arc::new(compiler), 2, 2).unwrap();
    let mut coordinator = Coordinator::with_limits(2, 2, 4, 1024);
    for id in 1..=2 {
        coordinator
            .queue(FunctionKey::new(id, 1), Tier::Baseline, snapshot(id, 1))
            .unwrap();
        assert!(workers.dispatch_next(&mut coordinator).unwrap());
    }

    assert!(control
        .request_within(std::time::Duration::from_secs(1))
        .is_some());
    assert!(
        control
            .request_within(std::time::Duration::from_millis(100))
            .is_some(),
        "the receiver mutex serialized all configured workers"
    );
    control.complete(CompiledArtifact::fake(Tier::Baseline));
    control.complete(CompiledArtifact::fake(Tier::Baseline));
    workers.shutdown(&mut coordinator);
}

#[test]
fn stale_completion_after_reload_is_discarded() {
    let (compiler, control) = FakeCompiler::new(1);
    let mut workers = BackgroundCompiler::new(Arc::new(compiler), 1, 1).unwrap();
    let mut coordinator = Coordinator::with_limits(2, 2, 4, 1024);
    let old = FunctionKey::new(11, 1);
    coordinator
        .queue(old, Tier::Baseline, snapshot(11, 1))
        .unwrap();
    workers.dispatch_next(&mut coordinator).unwrap();
    assert_eq!(control.next_request().unwrap().key(), old);

    let new = FunctionKey::new(11, 2);
    coordinator.retire(old);
    coordinator
        .queue(new, Tier::Baseline, snapshot(11, 2))
        .unwrap();
    control.complete(CompiledArtifact::fake(Tier::Baseline));
    workers.shutdown(&mut coordinator);

    assert_eq!(coordinator.state(old), CompileState::Retired);
    assert_eq!(coordinator.state(new), CompileState::Queued(Tier::Baseline));
    assert_eq!(coordinator.metrics().stale_results, 1);
}

#[test]
fn busy_workers_leave_backlog_queued_without_saturating_the_mailbox() {
    let (compiler, control) = FakeCompiler::new(2);
    let mut workers = BackgroundCompiler::new(Arc::new(compiler), 1, 1).unwrap();
    let mut coordinator = Coordinator::with_limits(3, 3, 4, 1024);
    for id in 1..=3 {
        coordinator
            .queue(FunctionKey::new(id, 1), Tier::Baseline, snapshot(id, 1))
            .unwrap();
    }
    assert!(workers.dispatch_next(&mut coordinator).unwrap());
    let _first = control.next_request().unwrap();
    // The only worker is busy: the backlog stays in the coordinator queue,
    // where a later Tier2 request can still overtake it.
    assert!(!workers.dispatch_next(&mut coordinator).unwrap());
    assert_eq!(coordinator.metrics().worker_queue_saturated, 0);
    assert_eq!(
        coordinator.state(FunctionKey::new(2, 1)),
        CompileState::Queued(Tier::Baseline)
    );

    control.complete(CompiledArtifact::fake(Tier::Baseline));
    while workers.live_usage().0 != 0 {
        std::thread::yield_now();
    }
    assert!(workers.dispatch_next(&mut coordinator).unwrap());
    assert_eq!(
        control.next_request().unwrap().key(),
        FunctionKey::new(2, 1)
    );
    control.complete(CompiledArtifact::fake(Tier::Baseline));
    workers.shutdown(&mut coordinator);
    assert_eq!(
        coordinator.state(FunctionKey::new(3, 1)),
        CompileState::Queued(Tier::Baseline)
    );
}

#[test]
fn shutdown_cooperatively_cancels_a_blocked_compiler() {
    let (compiler, control) = FakeCompiler::new(1);
    let mut workers = BackgroundCompiler::new(Arc::new(compiler), 1, 1).unwrap();
    let mut coordinator = Coordinator::with_limits(1, 1, 4, 1024);
    coordinator
        .queue(FunctionKey::new(19, 1), Tier::Baseline, snapshot(19, 1))
        .unwrap();
    workers.dispatch_next(&mut coordinator).unwrap();
    assert!(control.next_request().is_some());
    workers.shutdown(&mut coordinator);
    assert_eq!(coordinator.metrics().compile_failures, 1);
}

#[test]
fn saturated_completion_mailbox_does_not_deadlock_shutdown() {
    let (compiler, control) = FakeCompiler::new(2);
    let mut workers = BackgroundCompiler::new(Arc::new(compiler), 1, 2).unwrap();
    let mut coordinator = Coordinator::with_limits(2, 1, 4, 1024);
    for id in 31..=32 {
        coordinator
            .queue(FunctionKey::new(id, 1), Tier::Baseline, snapshot(id, 1))
            .unwrap();
        // Dispatch waits for an idle worker; the previous job may still be
        // releasing its usage after the fake compiler returned.
        while workers.live_usage().0 != 0 {
            std::thread::yield_now();
        }
        assert!(workers.dispatch_next(&mut coordinator).unwrap());
        assert!(control.next_request().is_some());
        control.complete(CompiledArtifact::fake(Tier::Baseline));
    }
    let deadline = Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while coordinator.metrics().completion_queue_saturated == 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(coordinator.metrics().completion_queue_saturated, 1);
    workers.shutdown(&mut coordinator);
    assert_eq!(coordinator.metrics().completion_queue_saturated, 1);
    assert_eq!(coordinator.metrics().installed, 2);
}

#[test]
fn thousand_hot_reload_completions_stay_generation_isolated() {
    let (compiler, control) = FakeCompiler::new(1);
    let mut workers = BackgroundCompiler::new(Arc::new(compiler), 1, 1).unwrap();
    let mut coordinator = Coordinator::with_limits(1, 1, 4, 1024);
    for generation in 1..=1_000 {
        let key = FunctionKey::new(77, generation);
        coordinator
            .queue(key, Tier::Baseline, snapshot(77, generation))
            .unwrap();
        workers.dispatch_next(&mut coordinator).unwrap();
        assert_eq!(control.next_request().unwrap().key(), key);
        control.complete(CompiledArtifact::fake(Tier::Baseline));
        while coordinator.drain_completions().drained() == 0 {
            std::thread::yield_now();
        }
        assert_eq!(
            coordinator.state(key),
            CompileState::Installed(Tier::Baseline)
        );
        coordinator.retire(key);
    }
    workers.shutdown(&mut coordinator);
    assert_eq!(coordinator.metrics().installed, 1_000);
    assert_eq!(coordinator.metrics().retired, 1_000);
}

#[cfg(all(
    feature = "compiler",
    any(
        all(
            target_os = "macos",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "windows",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "linux",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
#[test]
fn production_backend_receives_owned_snapshots_automatically() {
    let runtime = Runtime::new().unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let value: i32 = ctx
            .eval("globalThis.add = function add(a, b) { return a + b; }; let v=0; for(let i=0;i<32;i++) v=add(20,22); v")
            .unwrap();
        assert_eq!(value, 42);
    });
    let deadline = Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while Instant::now() < deadline {
        jit.poll();
        if jit.metrics().installed >= 1 {
            break;
        }
        std::thread::yield_now();
    }
    assert!(jit.metrics().queued >= 1);
    assert!(jit.metrics().installed >= 1, "{:?}", jit.metrics());
    context.with(|ctx| {
        let value: i32 = ctx.eval("add(20, 22)").unwrap();
        assert_eq!(value, 42);
    });
    let metrics = jit.metrics();
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.native_entries, metrics.native_exits);
    assert_eq!(metrics.native_fallbacks, 0);
    assert_eq!(metrics.native_retries, 0);
}

#[cfg(all(
    feature = "compiler",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn eight_short_callbacks_do_not_request_a_snapshot() {
    let runtime = Runtime::new().unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let value: i32 = ctx
            .eval("function f(x){return x+1}; let v=0; for(let i=0;i<8;i++)v=f(i); v")
            .unwrap();
        assert_eq!(value, 8);
    });
    for _ in 0..8 {
        jit.poll();
    }
    assert_eq!(jit.metrics().queued, 0, "{:?}", jit.metrics());
}

#[cfg(all(
    feature = "compiler",
    any(
        all(
            target_os = "macos",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "windows",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "linux",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
#[test]
fn two_production_runtimes_compile_install_execute_and_retire_independently() {
    // This is a runtime/cache isolation test, so keep the installed baseline
    // artifacts resident while their ownership and retirement are inspected.
    // The default automatic policy may demote an unprofitable baseline after
    // bounded Tier2 trials, making residency workload-dependent.
    let config = rquickjs_jit::JitConfig::builder()
        .tier_policy(JitTierPolicy::BaselineOnly)
        .build()
        .unwrap();
    let runtime_a = Runtime::new().unwrap();
    let jit_a = rquickjs_jit::Jit::attach(&runtime_a, config.clone()).unwrap();
    let context_a = Context::full(&runtime_a).unwrap();
    let runtime_b = Runtime::new().unwrap();
    let jit_b = rquickjs_jit::Jit::attach(&runtime_b, config).unwrap();
    let context_b = Context::full(&runtime_b).unwrap();

    assert!(!jit_a.test_artifact_environment_initialized());
    assert!(!jit_b.test_artifact_environment_initialized());

    context_a.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.f = function f(a, b) { return a + b }; for(let i=0;i<32;i++) f(1, 2)",
        )
        .unwrap()
    });
    context_b.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.f = function f(a, b) { return b + a }; for(let i=0;i<32;i++) f(1, 2)",
        )
        .unwrap()
    });
    let environment_a = jit_a.test_artifact_environment();
    let environment_b = jit_b.test_artifact_environment();
    assert_ne!(environment_a.runtime_id, environment_b.runtime_id);
    let deadline = Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while Instant::now() < deadline
        && (jit_a.metrics().installed == 0 || jit_b.metrics().installed == 0)
    {
        jit_a.poll();
        jit_b.poll();
        std::thread::yield_now();
    }
    assert!(jit_a.metrics().installed > 0, "A: {:?}", jit_a.metrics());
    assert!(jit_b.metrics().installed > 0, "B: {:?}", jit_b.metrics());

    for value in 0..64 {
        let a: i32 = context_a
            .with(|ctx| ctx.eval(format!("f({value}, 1)")))
            .unwrap();
        let b: i32 = context_b
            .with(|ctx| ctx.eval(format!("f({value}, 1)")))
            .unwrap();
        assert_eq!(a, value + 1);
        assert_eq!(b, value + 1);
    }
    let a_metrics = jit_a.metrics();
    let b_metrics = jit_b.metrics();
    assert!(
        a_metrics.code_bytes > 0 && b_metrics.code_bytes > 0,
        "A: {a_metrics:?}, B: {b_metrics:?}"
    );
    assert!(a_metrics.native_entries > 0 && b_metrics.native_entries > 0);
    assert_eq!(a_metrics.native_entries, a_metrics.native_exits);
    assert_eq!(b_metrics.native_entries, b_metrics.native_exits);
    let ordered_a: String = context_a
        .with(|ctx| ctx.eval("f('left', 'right')"))
        .unwrap();
    let ordered_b: String = context_b
        .with(|ctx| ctx.eval("f('left', 'right')"))
        .unwrap();
    assert_eq!(ordered_a, "leftright");
    assert_eq!(ordered_b, "rightleft");
    let key_a = jit_a.test_last_acquired_artifact_key().unwrap();
    let key_b = jit_b.test_last_acquired_artifact_key().unwrap();
    assert_eq!(key_a.runtime_id, environment_a.runtime_id);
    assert_eq!(key_b.runtime_id, environment_b.runtime_id);
    assert_ne!(key_a, key_b);

    drop(context_a);
    drop(jit_a);
    drop(runtime_a);
    for value in 64..128 {
        let b: i32 = context_b
            .with(|ctx| ctx.eval(format!("f({value}, 1)")))
            .unwrap();
        assert_eq!(b, value + 1);
    }
    let surviving = jit_b.metrics();
    assert!(surviving.native_entries > b_metrics.native_entries);
    assert_eq!(surviving.native_entries, surviving.native_exits);
}

#[cfg(all(
    feature = "compiler",
    any(
        all(
            target_os = "macos",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "windows",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "linux",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
#[test]
fn pending_job_poll_installs_without_an_additional_eligible_function_call() {
    let runtime = Runtime::new().unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.f = function f(a, b) { return a + b; };\n\
             for(let i=0;i<32;i++) f(1, 2); Promise.resolve().then(() => 7);",
        )
        .unwrap();
    });
    let deadline = Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while Instant::now() < deadline && jit.metrics().installed == 0 {
        let _ = runtime.execute_pending_job();
        std::thread::yield_now();
    }
    assert!(jit.metrics().installed > 0, "{:?}", jit.metrics());
}

#[cfg(all(
    feature = "compiler",
    any(
        all(
            target_os = "macos",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "windows",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "linux",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
#[test]
fn generator_resume_keeps_jit_stack_capacity_initialized() {
    let runtime = Runtime::new().unwrap();
    let _jit = rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let value: i32 = ctx
            .eval("function* g(){ yield 20; return 22 } const i=g(); i.next().value+i.next().value")
            .unwrap();
        assert_eq!(value, 42);
    });
}

#[cfg(all(
    feature = "compiler",
    any(
        all(
            target_os = "macos",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "windows",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "linux",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
#[test]
fn snapshot_quota_falls_back_without_disabling_runtime() {
    let runtime = Runtime::new().unwrap();
    let config = rquickjs_jit::JitConfig::builder()
        .max_snapshot_bytes(1)
        .build()
        .unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, config).unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let value: i32 = ctx
            .eval("let f=(function(a) { return a + 1; }); let v=0; for(let i=0;i<32;i++)v=f(41); v")
            .unwrap();
        assert_eq!(value, 42);
    });
    assert_eq!(jit.metrics().queued, 0);
    assert!(jit.metrics().resource_limit_rejections >= 1);
}

#[test]
fn pending_snapshot_and_ir_quotas_are_total_and_release_via_raii() {
    let (compiler, control) = FakeCompiler::new(1);
    let bytes = snapshot(1, 1).snapshot().owned_bytes();
    // Two workers, so the second request is dispatchable while the first is
    // pending and reaches the total quota check.
    let mut workers = BackgroundCompiler::new_with_resource_limits(
        Arc::new(compiler),
        2,
        2,
        std::time::Duration::from_secs(30),
        bytes,
        bytes * 32,
    )
    .unwrap();
    let mut coordinator = Coordinator::with_limits(2, 2, 4, 1024);
    coordinator
        .queue(FunctionKey::new(1, 1), Tier::Baseline, snapshot(1, 1))
        .unwrap();
    coordinator
        .queue(FunctionKey::new(2, 1), Tier::Baseline, snapshot(2, 1))
        .unwrap();
    assert!(workers.dispatch_next(&mut coordinator).unwrap());
    assert!(control.next_request().is_some());
    assert!(!workers.dispatch_next(&mut coordinator).unwrap());
    assert_eq!(coordinator.metrics().resource_limit_rejections, 1);
    control.complete(CompiledArtifact::fake(Tier::Baseline));
    while workers.live_usage().0 != 0 {
        std::thread::yield_now();
    }
    assert!(workers.dispatch_next(&mut coordinator).unwrap());
    assert!(control.next_request().is_some());
    control.complete(CompiledArtifact::fake(Tier::Baseline));
    workers.shutdown(&mut coordinator);
    assert_eq!(workers.live_usage(), (0, 0, 0));
}

#[test]
fn compile_deadline_is_categorized_and_interpretation_remains_available() {
    let (compiler, _control) = FakeCompiler::new(1);
    let mut workers = BackgroundCompiler::new_with_resource_limits(
        Arc::new(compiler),
        1,
        1,
        std::time::Duration::ZERO,
        usize::MAX,
        usize::MAX,
    )
    .unwrap();
    let mut coordinator = Coordinator::with_limits(1, 1, 4, 1024);
    let key = FunctionKey::new(55, 1);
    coordinator
        .queue(key, Tier::Baseline, snapshot(55, 1))
        .unwrap();
    workers.dispatch_next(&mut coordinator).unwrap();
    while workers.live_usage().0 != 0 {
        std::thread::yield_now();
    }
    while coordinator.drain_completions().drained() == 0 {
        std::thread::yield_now();
    }
    workers.shutdown(&mut coordinator);
    assert_eq!(coordinator.metrics().compile_timeouts, 1);
    assert!(matches!(
        coordinator.state(key),
        CompileState::Backoff { .. }
    ));
}
