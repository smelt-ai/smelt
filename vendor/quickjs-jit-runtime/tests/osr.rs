use rquickjs::{Context, Runtime};
use rquickjs_core::runtime::JitBackend;
use rquickjs_jit::bytecode::SlotKind;
use rquickjs_jit::compiler::baseline::BaselineCompiler;
use rquickjs_jit::runtime::{FunctionKey, OsrKey, OsrMap};
use rquickjs_jit::test_support::SnapshotFixture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[cfg(all(
    feature = "compiler",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
unsafe extern "C" {
    fn JS_JitSetExecutionTrace(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        events: *mut rquickjs_core::qjs::JSJitTraceEvent,
        capacity: u32,
    ) -> i32;
    fn JS_JitGetExecutionTraceLength(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        length: *mut u32,
        overflowed: *mut i32,
    ) -> i32;
}

#[test]
fn osr_map_retains_exact_function_pc_depth_and_slot_kinds() {
    let key = OsrKey::new(FunctionKey::new(9, 3), 17);
    let map = OsrMap::new(key, 41, 2, vec![SlotKind::Tagged, SlotKind::Int32]);
    assert_eq!(map.key(), key);
    assert_eq!(map.entry_offset(), 41);
    assert_eq!(map.stack_depth(), 2);
    assert_eq!(map.live_slots(), [SlotKind::Tagged, SlotKind::Int32]);
    assert!(map.matches(
        FunctionKey::new(9, 3),
        17,
        2,
        &[SlotKind::Tagged, SlotKind::Int32]
    ));
    assert!(!map.matches(
        FunctionKey::new(9, 4),
        17,
        2,
        &[SlotKind::Tagged, SlotKind::Int32]
    ));
    assert!(!map.matches(
        FunctionKey::new(9, 3),
        18,
        2,
        &[SlotKind::Tagged, SlotKind::Int32]
    ));
    assert!(!map.matches(
        FunctionKey::new(9, 3),
        17,
        1,
        &[SlotKind::Tagged, SlotKind::Int32]
    ));
}

#[test]
fn verifier_derives_osr_maps_for_each_real_loop_header() {
    let fixture = SnapshotFixture::compile(
        "(function f(n, zero){let s=zero;for(let i=zero;i<n;i++)s+=i;return s})",
    );
    let verified = fixture.snapshot().verify(Default::default()).unwrap();
    assert!(!verified.osr_points().is_empty());
    for point in verified.osr_points() {
        assert!(verified.control_flow_graph().is_loop_header(point.pc()));
        assert!(
            point.live_slots().len()
                >= verified.snapshot().arg_count() as usize
                    + verified.snapshot().local_count() as usize
        );
    }
}

#[test]
fn osr_maps_are_built_only_from_verified_headers_and_independent_entries() {
    let fixture = SnapshotFixture::compile(
        "(function f(n, zero){let s=zero;for(let i=zero;i<n;i++)s+=i;return s})",
    );
    let verified = fixture.snapshot().verify(Default::default()).unwrap();
    let point = &verified.osr_points()[0];
    let map = OsrMap::from_verified(&verified, point.pc(), 123).unwrap();
    assert_eq!(map.entry_offset(), 123);
    assert_eq!(map.key().pc(), point.pc());
    assert_eq!(map.live_slots(), point.live_slots());
    assert!(OsrMap::from_verified(&verified, point.pc().saturating_add(1), 123).is_none());
    assert!(
        OsrMap::from_verified(&verified, point.pc(), 0).is_none(),
        "function entry may not masquerade as OSR"
    );
}

#[test]
fn tier1_artifact_charges_independent_osr_code() {
    let fixture = SnapshotFixture::compile(
        "(function f(n, zero){let s=zero;for(let i=zero;i<n;i++)s+=i;return s})",
    );
    let verified = fixture.snapshot().verify(Default::default()).unwrap();
    let names: Vec<_> = verified
        .instructions()
        .iter()
        .map(|op| op.opcode().name())
        .collect();
    let code = BaselineCompiler::host()
        .compile(&verified)
        .unwrap_or_else(|error| panic!("{error:?}: {names:?}"));
    assert!(code.osr_entry_count() >= 1);
    assert!(code.total_code_bytes() > code.bytes().len());
}

#[derive(Default)]
struct EventState {
    calls: HashMap<(u64, u64), u32>,
    loops: u32,
    snapshots: u32,
    loop_pcs: Vec<u32>,
    hotness: HashMap<(u64, u64), rquickjs_jit::runtime::HotnessState>,
}

struct EventBackend(Arc<Mutex<EventState>>);

unsafe impl JitBackend for EventBackend {
    fn record_hot(&mut self, event: &rquickjs_core::qjs::JSJitHotEvent) -> u32 {
        let mut state = self.0.lock().unwrap();
        let key = (event.function.id, event.function.generation);
        if event.kind == rquickjs_core::qjs::JSJitHotKind_JS_JIT_HOT_CALL {
            *state.calls.entry(key).or_default() = state
                .calls
                .get(&key)
                .copied()
                .unwrap_or(0)
                .saturating_add(event.count);
            return u32::from(matches!(
                state
                    .hotness
                    .entry(key)
                    .or_default()
                    .record_call_event(event.count),
                rquickjs_jit::runtime::HotDecision::Queue(_)
            ));
        }
        state.loops = state.loops.saturating_add(event.count);
        state.loop_pcs.push(event.pc);
        u32::from(matches!(
            state
                .hotness
                .entry(key)
                .or_default()
                .record_loop_event(event.count),
            rquickjs_jit::runtime::HotDecision::Queue(_)
        ))
    }

    fn submit_snapshot(&mut self, snapshot: *mut rquickjs_core::qjs::JSJitFunctionSnapshot) {
        self.0.lock().unwrap().snapshots += 1;
        unsafe { rquickjs_core::qjs::JS_JitFreeSnapshot(snapshot) };
    }
}

#[test]
fn taken_backedges_report_after_interrupt_and_request_one_snapshot() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let state = Arc::new(Mutex::new(EventState::default()));
    let _guard = runtime
        .attach_jit_backend(EventBackend(Arc::clone(&state)))
        .unwrap();
    context.with(|ctx| {
        assert_eq!(
            ctx.eval::<i32, _>("function f(){let s=0;for(let i=0;i<80;i++)s+=i;return s} f()")
                .unwrap(),
            3160
        );
    });
    let state = state.lock().unwrap();
    assert!(state.calls.values().any(|calls| *calls == 1));
    assert!(state.loops >= 56);
    assert_eq!(state.snapshots, 1);
    assert!(state.loop_pcs.iter().all(|pc| *pc != 0));
}

#[test]
fn untaken_loop_reports_no_backedge() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let state = Arc::new(Mutex::new(EventState::default()));
    let _guard = runtime
        .attach_jit_backend(EventBackend(Arc::clone(&state)))
        .unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>("function f(){for(let i=0;i<0;i++){} } f()")
            .unwrap()
    });
    assert_eq!(state.lock().unwrap().loops, 0);
}

#[cfg(all(
    feature = "compiler",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn long_first_invocation_enters_a_real_production_osr_entry() {
    let runtime = Runtime::new().unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    let result = context.with(|ctx| {
        ctx.eval::<f64, _>(
            "function f(n,z){let s=z;for(let i=z;i<n;i++)s+=i;return s} f(5000000,0)",
        )
    });
    let value = result.unwrap_or_else(|error| panic!("{error:?}; {:?}", jit.metrics()));
    assert_eq!(value, 12_499_997_500_000.0);
    assert!(jit.metrics().osr_entries >= 1, "{:?}", jit.metrics());
    assert_eq!(jit.metrics().native_retries, 0, "{:?}", jit.metrics());
}

#[test]
fn compiler_emits_one_independent_entry_per_verified_loop_header() {
    let fixture = SnapshotFixture::compile(
        "(function f(n,z){let a=z,i=z;while(i<n){while(a<z)a++;a+=i;i++}return a})",
    );
    let verified = fixture.snapshot().verify(Default::default()).unwrap();
    let expected = verified.osr_points().len();
    assert!(expected >= 2);
    let names: Vec<_> = verified
        .instructions()
        .iter()
        .map(|op| op.opcode().name())
        .collect();
    let code = BaselineCompiler::host()
        .compile(&verified)
        .unwrap_or_else(|error| panic!("{error:?}: {names:?}"));
    assert_eq!(code.osr_entry_count(), expected);
}

#[cfg(all(
    feature = "compiler",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn counted_loop(jit: bool) -> (f64, usize, u64) {
    let runtime = Runtime::new().unwrap();
    let attached = jit
        .then(|| rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap());
    let polls = Arc::new(AtomicUsize::new(0));
    runtime.set_interrupt_handler({
        let polls = Arc::clone(&polls);
        Some(Box::new(move || {
            polls.fetch_add(1, Ordering::SeqCst);
            false
        }))
    });
    let context = Context::full(&runtime).unwrap();
    let value = context.with(|ctx| {
        ctx.eval::<f64, _>(
            "function f(n,z){let s=z;for(let i=z;i<n;i++)s+=i;return s} f(5000000,0)",
        )
        .unwrap()
    });
    let entries = attached.as_ref().map_or(0, |jit| jit.metrics().osr_entries);
    (value, polls.load(Ordering::SeqCst), entries)
}

#[cfg(all(
    feature = "compiler",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn osr_countdown_preserves_result_and_interrupt_service() {
    let interpreted = counted_loop(false);
    let native = counted_loop(true);
    assert_eq!(native.0, interpreted.0);
    assert!(native.2 >= 1);
    assert!(
        native.1 > 0,
        "native loops must continue servicing interrupts"
    );
    assert!(
        native.1 <= interpreted.1,
        "the native countdown must amortize, not amplify, interrupt callbacks"
    );
}

#[cfg(all(
    feature = "compiler",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn branched_loop_osr_preserves_result_and_interrupt_service() {
    fn run(jit_enabled: bool) -> (i32, usize, u64) {
        let runtime = Runtime::new().unwrap();
        let attached = jit_enabled.then(|| {
            rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap()
        });
        let polls = Arc::new(AtomicUsize::new(0));
        runtime.set_interrupt_handler({
            let polls = Arc::clone(&polls);
            Some(Box::new(move || {
                polls.fetch_add(1, Ordering::SeqCst);
                false
            }))
        });
        let context = Context::full(&runtime).unwrap();
        let value = context.with(|ctx| {
            ctx.eval::<i32, _>("let state={limit:500000,half:250000}; function f(state,z){let i=z;while(i<state.limit){if(i<state.half){i++;continue}i++}return i} f(state,0)").unwrap()
        });
        (
            value,
            polls.load(Ordering::SeqCst),
            attached.map_or(0, |jit| jit.metrics().osr_entries),
        )
    }
    let interpreted = run(false);
    let native = run(true);
    assert_eq!(native.0, interpreted.0);
    assert!(native.2 > 0);
    assert!(
        native.1 > 0,
        "branched native loops must service interrupts"
    );
    assert!(
        native.1 <= interpreted.1,
        "branched native loops must not amplify interrupt callbacks"
    );
}

#[cfg(all(
    feature = "compiler",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn hot_reload_retires_old_osr_entries_without_cross_generation_execution() {
    let runtime = Runtime::new().unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, rquickjs_jit::JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    let first: f64 = context.with(|ctx| {
        ctx.eval(
            "globalThis.f=function(n,z){let s=z;for(let i=z;i<n;i++)s+=i;return s}; f(5000000,0)",
        )
        .unwrap()
    });
    let first_entries = jit.metrics().osr_entries;
    assert_eq!(first, 12_499_997_500_000.0);
    assert!(first_entries >= 1);
    let second: f64 = context.with(|ctx| {
        ctx.eval(
            "globalThis.f=function(n,z){let s=z;for(let i=z;i<n;i++)s+=i;return s+1}; f(5000000,0)",
        )
        .unwrap()
    });
    assert_eq!(second, 12_499_997_500_001.0);
    assert!(
        jit.metrics().osr_entries > first_entries,
        "{:?}",
        jit.metrics()
    );
    assert_eq!(jit.metrics().native_retries, 0);
}

#[cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn first_invocation_osr_executes_helper_with_side_effect_gc_and_reentry() {
    let fixture = SnapshotFixture::compile(
        "(function f(state,z){let s=state.next;for(let i=z;i<state.limit;i++){s=state.next}return s})",
    );
    let verified = fixture.snapshot().verify(Default::default()).unwrap();
    BaselineCompiler::host()
        .compile(&verified)
        .unwrap_or_else(|error| {
            panic!(
                "{error:?}: {:?}",
                verified
                    .instructions()
                    .iter()
                    .map(|i| i.opcode().name())
                    .collect::<Vec<_>>()
            )
        });
    let runtime = Runtime::new().unwrap();
    let config = rquickjs_jit::JitConfig::builder()
        // This is a Tier1 OSR correctness test, not an Automatic profitability
        // test.  Keep the baseline artifact published while the first long
        // invocation reaches its loop probe; Automatic may legitimately
        // demote helper-heavy code after its bounded profitability trials.
        .tier_policy(rquickjs_jit::JitTierPolicy::BaselineOnly)
        // This test exercises the loop-triggered OSR request.  Suppress
        // independent callback requests so the one worker cannot be occupied
        // compiling unrelated short helper functions first.
        .call_threshold(u32::MAX)
        .stress_gc(true)
        .build()
        .unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, config).unwrap();
    let context = Context::full(&runtime).unwrap();
    let rt =
        context.with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
    assert_eq!(
        unsafe { rquickjs_core::qjs::JS_JitResetHelperCounters(rt) },
        0
    );
    let mut trace =
        vec![unsafe { core::mem::zeroed::<rquickjs_core::qjs::JSJitTraceEvent>() }; 1_000_000];
    assert_eq!(
        unsafe { JS_JitSetExecutionTrace(rt, trace.as_mut_ptr(), trace.len() as u32) },
        0
    );
    let result = context.with(|ctx| {
        // A faster interpreter can finish this first invocation while an
        // instrumented compiler is still producing its OSR child. Yield only
        // until native helpers have actually run, rather than racing a fixed
        // iteration count against worker scheduling. Keep a bounded deadline
        // so broken OSR still fails instead of hanging the test.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 150 } else { 30 });
        ctx.globals().set("paceUntilNative", rquickjs::function::Func::from(
            move |ctx: rquickjs::Ctx<'_>| -> rquickjs::Result<()> {
                let mut counters: rquickjs_core::qjs::JSJitHelperCounters =
                    unsafe { core::mem::zeroed() };
                counters.struct_size = core::mem::size_of_val(&counters) as u32;
                let rt = unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
                assert_eq!(unsafe {
                    rquickjs_core::qjs::JS_JitGetHelperCounters(rt, &mut counters)
                }, 0);
                if counters.dup_count == 0 && counters.free_count == 0 {
                    if std::time::Instant::now() >= deadline {
                        return Err(rquickjs::Exception::throw_message(
                            &ctx, "timed out waiting for first-invocation OSR",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(())
            },
        )).unwrap();
        ctx.eval::<i32, _>(
            "let events=0,reentered=0; function nested(){reentered++} let state={limit:50000,get next(){paceUntilNative();events++;nested();return events}}; function f(state,z){let s=state.next;for(let i=z;i<state.limit;i++){s=state.next}return s} f(state,0)",
        )
    }).unwrap_or_else(|error| panic!("{error:?}; {:?}", jit.metrics()));
    assert_eq!(result, 50001);
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("events").unwrap()),
        50001
    );
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("reentered").unwrap()),
        50001
    );
    let mut counters: rquickjs_core::qjs::JSJitHelperCounters = unsafe { core::mem::zeroed() };
    counters.struct_size = core::mem::size_of_val(&counters) as u32;
    assert_eq!(
        unsafe { rquickjs_core::qjs::JS_JitGetHelperCounters(rt, &mut counters) },
        0
    );
    let mut trace_len = 0;
    let mut overflowed = 0;
    assert_eq!(
        unsafe { JS_JitGetExecutionTraceLength(rt, &mut trace_len, &mut overflowed) },
        0
    );
    assert_eq!(overflowed, 0);
    trace.truncate(trace_len as usize);
    let helper = rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY as u16;
    assert!(
        trace
            .iter()
            .any(|event| event.kind == 1 && u16::from(event.helper_id) == helper),
        "OSR child did not execute GET_PROPERTY helper: {:?}",
        jit.metrics()
    );
    assert!(counters.dup_count > 0 || counters.free_count > 0);
    let metrics = jit.metrics();
    assert!(metrics.osr_validated_successes > 0, "{metrics:?}");
    assert_eq!(metrics.osr_generated_retries, 0, "{metrics:?}");
    assert_eq!(metrics.osr_validation_failures, 0, "{metrics:?}");
    assert_eq!(
        unsafe { JS_JitSetExecutionTrace(rt, core::ptr::null_mut(), 0) },
        0
    );
}

#[cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_ineligible_loop_is_rejected_once_before_queueing() {
    let failure_fixture =
        SnapshotFixture::compile("(function f(n,z){let i=z;while(i<n)i++;return String(1n)})");
    let verified = failure_fixture
        .snapshot()
        .verify(Default::default())
        .unwrap();
    assert!(verified.tier1_eligibility().is_err());
    assert!(BaselineCompiler::host().compile(&verified).is_err());
    let runtime = Runtime::new().unwrap();
    let config = rquickjs_jit::JitConfig::builder()
        .loop_threshold(1)
        .max_compile_attempts(4)
        .build()
        .unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, config).unwrap();
    let context = Context::full(&runtime).unwrap();
    let value = context.with(|ctx| {
        ctx.eval::<String, _>(
            "function f(n,z){let i=z;while(i<n)i++;return String(1n)} f(1000000,0)",
        )
        .unwrap()
    });
    assert_eq!(value, "1");
    let metrics = jit.metrics();
    assert_eq!(metrics.queued, 0, "ineligible work was queued: {metrics:?}");
    assert_eq!(metrics.compile_failures, 1, "{metrics:?}");
    assert_eq!(metrics.tier1_rejections, 1, "{metrics:?}");
    assert_eq!(metrics.blacklisted, 1, "{metrics:?}");
    assert_eq!(metrics.hot_loop_queues, 0, "{metrics:?}");
    assert_eq!(
        metrics.snapshot_requests, 1,
        "terminal eligibility must suppress duplicate snapshots: {metrics:?}"
    );
}

#[cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_snapshot_rejection_uses_bounded_prequeue_backoff() {
    let runtime = Runtime::new().unwrap();
    let config = rquickjs_jit::JitConfig::builder()
        .loop_threshold(1)
        .max_snapshot_bytes(1)
        .build()
        .unwrap();
    let jit = rquickjs_jit::Jit::attach(&runtime, config).unwrap();
    let context = Context::full(&runtime).unwrap();
    assert_eq!(
        context.with(|ctx| ctx
            .eval::<i32, _>("function f(n,z){let i=z;while(i<n)i++;return i} f(10000,0)")
            .unwrap()),
        10000
    );
    let metrics = jit.metrics();
    assert_eq!(metrics.queued, 0, "{metrics:?}");
    assert!(
        metrics.resource_limit_rejections > 1,
        "transient path never retried: {metrics:?}"
    );
    assert!(
        metrics.snapshot_requests < 32,
        "prequeue failure spun duplicate snapshots: {metrics:?}"
    );
    assert_eq!(
        metrics.snapshot_requests, metrics.resource_limit_rejections,
        "{metrics:?}"
    );
    assert_eq!(
        metrics.hot_loop_queues, 0,
        "rejected snapshots were not coordinator queues: {metrics:?}"
    );
    assert_eq!(metrics.adaptive_neutral_queues, 0, "{metrics:?}");
    assert_eq!(
        metrics.adaptive_inputs_recorded, 0,
        "copy-size rejection precedes verified inputs"
    );
}
