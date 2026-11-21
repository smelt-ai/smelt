//! ABI 1.25 callback-free native entry: QuickJS may run a cached, granted
//! handle without per-call backend callbacks, but never past its budget, a
//! changed epoch, suspension, or a non-DONE exit.

use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};

use rquickjs::{Context, Function, Runtime};
use rquickjs_core::{qjs, runtime::JitBackend};
use rquickjs_jit::{abi::JitExitExt, bytecode::CompileSnapshot};

#[derive(Default)]
struct Calls {
    acquisitions: AtomicUsize,
    releases: AtomicUsize,
    enters: AtomicUsize,
    exits: Mutex<Vec<u32>>,
    grants: AtomicUsize,
    hot_events: AtomicUsize,
    call_feedback: AtomicUsize,
    executions: AtomicUsize,
    fast_executions: AtomicUsize,
    /// Execution number (1-based) that returns RETRY instead of DONE.
    retry_at: AtomicUsize,
}

/// Leaked for the process lifetime so native code can reach it without a pin.
/// Each allocation stays registered so LeakSanitizer still finds it reachable
/// once the test that created it has returned.
fn calls() -> &'static Calls {
    static LEAKED: Mutex<Vec<&'static Calls>> = Mutex::new(Vec::new());
    let calls: &'static Calls = Box::leak(Box::default());
    LEAKED.lock().unwrap().push(calls);
    calls
}

struct Pin {
    calls: &'static Calls,
}

unsafe extern "C" fn native_counted(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    let frame = unsafe { &mut *frame };
    let calls = unsafe { &*frame.entry.pin.cast::<Pin>() }.calls;
    let execution = calls.executions.fetch_add(1, Ordering::SeqCst) + 1;
    if frame.flags & qjs::JS_JIT_FRAME_FAST_ENTRY != 0 {
        calls.fast_executions.fetch_add(1, Ordering::SeqCst);
    }
    if calls.retry_at.load(Ordering::SeqCst) == execution {
        return qjs::JSJitExit::retry_interpreter();
    }
    frame.result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 42);
    qjs::JSJitExit::done()
}

struct StateBox(*mut qjs::JSJitFastEntryState);
// The state is only touched on the runtime thread and by the test after the
// calls return.
unsafe impl Send for StateBox {}

struct FastBackend {
    key: (u64, u64),
    calls: &'static Calls,
    epoch: Arc<AtomicU64>,
    budget: u32,
    state: StateBox,
    malformed_state: bool,
}

unsafe impl JitBackend for FastBackend {
    fn record_hot(&mut self, _event: &qjs::JSJitHotEvent) -> u32 {
        self.calls.hot_events.fetch_add(1, Ordering::SeqCst);
        0
    }

    fn record_feedback(&mut self, event: &qjs::JSJitFeedbackEvent) {
        if event.kind == qjs::JSJitFeedbackKind_JS_JIT_FEEDBACK_CALL
            && event.flags & qjs::JS_JIT_FEEDBACK_CALL_SITE == 0
        {
            self.calls.call_feedback.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn acquire_entry(&mut self, id: u64, generation: u64, pc: u32) -> qjs::JSJitEntryHandle {
        let mut entry: qjs::JSJitEntryHandle = unsafe { std::mem::zeroed() };
        entry.struct_size = std::mem::size_of_val(&entry) as u32;
        if (id, generation) == self.key && pc == 0 {
            self.calls.acquisitions.fetch_add(1, Ordering::SeqCst);
            entry.entry = Some(native_counted);
            entry.helper_abi_version = qjs::QJSJIT_HELPER_ABI_VERSION;
            entry.pin = Box::into_raw(Box::new(Pin { calls: self.calls })).cast();
        }
        entry
    }

    fn release_entry(&mut self, entry: qjs::JSJitEntryHandle) {
        if !entry.pin.is_null() {
            unsafe { drop(Box::from_raw(entry.pin.cast::<Pin>())) };
            self.calls.releases.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn entry_cache_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn entry_fast_grant(&mut self, id: u64, generation: u64, grant: &mut qjs::JSJitFastEntryGrant) {
        assert_eq!((id, generation), self.key);
        self.calls.grants.fetch_add(1, Ordering::SeqCst);
        let state = unsafe { &mut *self.state.0 };
        state.epoch = self.epoch.load(Ordering::SeqCst);
        if self.malformed_state {
            state.struct_size = 1;
        }
        grant.budget = self.budget;
        grant.flags = qjs::JS_JIT_FAST_ENTRY_OPTIMIZED;
        grant.state = self.state.0;
    }

    fn native_enter(&mut self, _id: u64, _generation: u64, _pc: u32) {
        self.calls.enters.fetch_add(1, Ordering::SeqCst);
    }

    fn native_exit(&mut self, _id: u64, _generation: u64, _pc: u32, exit_kind: u32) {
        self.calls.exits.lock().unwrap().push(exit_kind);
    }
}

struct Harness {
    runtime: Runtime,
    context: Context,
    calls: &'static Calls,
    epoch: Arc<AtomicU64>,
    state: *mut qjs::JSJitFastEntryState,
    guard: Option<rquickjs_core::runtime::RuntimeJitGuard>,
}

impl Harness {
    fn new(budget: u32, malformed_state: bool) -> Self {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let captured = context.with(|ctx| {
            ctx.eval::<(), _>("globalThis.target = function target(x) { return 1 }")
                .unwrap();
            let function = ctx.globals().get::<_, Function>("target").unwrap();
            unsafe {
                CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_value().as_raw())
            }
            .unwrap()
        });
        let calls = calls();
        let epoch = Arc::new(AtomicU64::new(1));
        let state = Box::into_raw(Box::new(qjs::JSJitFastEntryState {
            struct_size: std::mem::size_of::<qjs::JSJitFastEntryState>() as u32,
            reserved: 0,
            epoch: 1,
            entries: 0,
            exits: 0,
            optimized_entries: 0,
        }));
        let guard = runtime
            .attach_jit_backend(FastBackend {
                key: (captured.function_id(), captured.generation()),
                calls,
                epoch: Arc::clone(&epoch),
                budget,
                state: StateBox(state),
                malformed_state,
            })
            .unwrap();
        Self {
            runtime,
            context,
            calls,
            epoch,
            state,
            guard: Some(guard),
        }
    }

    fn call(&self, expected: i32) {
        self.context.with(|ctx| {
            let function: Function = ctx.globals().get("target").unwrap();
            assert_eq!(function.call::<_, i32>((7,)).unwrap(), expected);
        });
    }

    fn state(&self) -> qjs::JSJitFastEntryState {
        unsafe { *self.state }
    }

    fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::SeqCst);
        unsafe { (*self.state).epoch = epoch };
    }

    fn enters(&self) -> usize {
        self.calls.enters.load(Ordering::SeqCst)
    }

    fn exits(&self) -> Vec<u32> {
        self.calls.exits.lock().unwrap().clone()
    }

    fn fast(&self) -> usize {
        self.calls.fast_executions.load(Ordering::SeqCst)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        drop(self.guard.take());
        let _ = &self.runtime;
        unsafe { drop(Box::from_raw(self.state)) };
    }
}

const DONE: u32 = qjs::JSJitExitKind_JS_JIT_EXIT_DONE;

#[test]
fn granted_calls_skip_every_callback_until_the_budget_expires() {
    let harness = Harness::new(3, false);
    for _ in 0..10 {
        harness.call(42);
    }
    let calls = harness.calls;
    // Calls 1, 5 and 9 take the full path (the first acquires); each grants
    // three callback-free executions.
    assert_eq!(calls.executions.load(Ordering::SeqCst), 10);
    assert_eq!(harness.fast(), 7);
    assert_eq!(harness.enters(), 3);
    assert_eq!(harness.exits(), vec![DONE; 3]);
    assert_eq!(calls.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(calls.grants.load(Ordering::SeqCst), 3);
    assert_eq!(calls.hot_events.load(Ordering::SeqCst), 3);
    assert_eq!(calls.call_feedback.load(Ordering::SeqCst), 3);
    let state = harness.state();
    assert_eq!(
        (state.entries, state.exits, state.optimized_entries),
        (7, 7, 7)
    );
    assert_eq!(calls.releases.load(Ordering::SeqCst), 0);
    drop(harness);
    assert_eq!(calls.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn an_epoch_change_stops_granted_calls_before_the_next_execution() {
    let harness = Harness::new(100, false);
    harness.call(42);
    harness.call(42);
    assert_eq!((harness.enters(), harness.fast()), (1, 1));
    harness.set_epoch(2);
    harness.call(42);
    // The stale handle is released and reacquired on the full path.
    assert_eq!((harness.enters(), harness.fast()), (2, 1));
    assert_eq!(harness.calls.acquisitions.load(Ordering::SeqCst), 2);
    assert_eq!(harness.calls.releases.load(Ordering::SeqCst), 1);
    harness.call(42);
    assert_eq!((harness.enters(), harness.fast()), (2, 2));
    // Zero disables fast calls outright even for a matching cached epoch.
    unsafe { (*harness.state).epoch = 0 };
    harness.call(42);
    assert_eq!(harness.fast(), 2);
    assert_eq!(harness.enters(), 3);
}

#[test]
fn a_non_done_fast_exit_reports_an_unpaired_native_exit_and_is_not_cached() {
    let harness = Harness::new(100, false);
    harness.calls.retry_at.store(3, Ordering::SeqCst);
    harness.call(42);
    harness.call(42);
    // Execution 3 is fast and retries in the interpreter, which returns 1.
    harness.call(1);
    assert_eq!(harness.fast(), 2);
    assert_eq!(
        harness.exits(),
        vec![
            DONE,
            qjs::JSJitExitKind_JS_JIT_EXIT_RETRY_INTERPRETER | qjs::JS_JIT_EXIT_FAST_UNPAIRED
        ]
    );
    let state = harness.state();
    assert_eq!((state.entries, state.exits), (2, 1));
    assert_eq!(harness.calls.releases.load(Ordering::SeqCst), 1);
    harness.call(42);
    assert_eq!(harness.calls.acquisitions.load(Ordering::SeqCst), 2);
    assert_eq!(harness.enters(), 2);
}

#[test]
fn suspension_and_malformed_grants_keep_the_full_callback_path() {
    let harness = Harness::new(100, false);
    harness.call(42);
    harness.guard.as_ref().unwrap().suspend().unwrap();
    harness.call(1);
    harness.guard.as_ref().unwrap().resume().unwrap();
    harness.call(42);
    assert_eq!(harness.fast(), 1);

    let oversized = Harness::new(qjs::JS_JIT_FAST_ENTRY_MAX_BUDGET + 1, false);
    let malformed = Harness::new(100, true);
    for harness in [&oversized, &malformed] {
        for _ in 0..4 {
            harness.call(42);
        }
        assert_eq!(harness.fast(), 0);
        assert_eq!(harness.enters(), 4);
        assert_eq!(harness.calls.acquisitions.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn calls_without_native_code_keep_another_functions_granted_handle() {
    let harness = Harness::new(100, false);
    harness.call(42);
    harness.context.with(|ctx| {
        // `other` never acquires native code; its call must neither run the
        // granted handle nor evict it before the nested `target` call.
        ctx.eval::<(), _>("globalThis.other = function other() { return target(1) + 1 }")
            .unwrap();
        let other: Function = ctx.globals().get("other").unwrap();
        assert_eq!(other.call::<_, i32>(()).unwrap(), 43);
    });
    assert_eq!(harness.fast(), 1);
    assert_eq!(harness.enters(), 1);
    assert_eq!(harness.calls.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(harness.calls.releases.load(Ordering::SeqCst), 0);
}

/// Production tiering: a steady Tier-2 sort comparator re-entered from C runs
/// callback-free while metrics stay exact, and a guard failure on a granted
/// call still leaves native code with the interpreter's result.
#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn steady_sort_comparator_uses_granted_entries_with_exact_metrics() {
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(&runtime, JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function compare(left, right) { return left.score - right.score; }\
                 function run(count, text) {\
                   const items = [];\
                   for (let i = 0; i < count; i += 1) {\
                     const score = (i * 7919) % 1000;\
                     items.push({ score: text ? String(score) : score });\
                   }\
                   items.sort(compare);\
                   return Number(items[0].score) + Number(items[count - 1].score) * 1000;\
                 }",
            )
        })
        .unwrap();
    let run = |text: bool| {
        context.with(|ctx| {
            let run: Function = ctx.globals().get("run").unwrap();
            run.call::<_, i32>((1_000, text)).unwrap()
        })
    };

    let mut steady = None;
    // Sanitizer builds compile Tier 2 far more slowly while this file's other
    // tests share the runner's cores.
    let budget = if cfg!(rquickjs_sanitizer) { 120 } else { 20 };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget);
    while std::time::Instant::now() < deadline {
        jit.poll();
        let before = jit.metrics();
        assert_eq!(run(false), 999_000);
        let after = jit.metrics();
        let entries = after.native_entries - before.native_entries;
        let acquisitions = after.native_acquisitions - before.native_acquisitions;
        let tier2 = after.tier2_entries - before.tier2_entries;
        // About 10k comparator calls. The per-call callback path reacquired
        // a handle about every 32 calls; a grant renews every 256.
        if entries >= 5_000 && tier2 >= 5_000 && acquisitions * 64 < entries {
            steady = Some((before, after));
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let (before, after) = steady.unwrap_or_else(|| panic!("never steady: {:?}", jit.metrics()));
    assert_eq!(after.native_entries, after.native_exits, "{after:?}");
    assert!(
        after.benefit_recordings - before.benefit_recordings
            < (after.native_entries - before.native_entries) / 64,
        "granted calls must not time every execution: before={before:?}, after={after:?}"
    );

    // String scores leave the numeric Tier-2 path on granted calls; the
    // interpreter must finish each comparison with the same answer.
    assert_eq!(run(true), 999_000);
    assert_eq!(run(false), 999_000);
    let last = jit.metrics();
    assert!(last.deopts > after.deopts, "{last:?}");
    assert_eq!(last.native_entries, last.native_exits, "{last:?}");
}
