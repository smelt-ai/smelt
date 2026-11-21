use std::{
    collections::HashMap,
    mem, ptr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

#[cfg(feature = "compiler")]
use rquickjs_core::runtime::JitFunctionRegistry;
use rquickjs_core::runtime::{JitBackend, JitBackendAttachError, RuntimeJitGuard};
#[cfg(feature = "compiler")]
use rquickjs_core::Function;
use rquickjs_core::{context::EvalOptions, Context, Runtime, Value};

use crate::abi::{AbiInfo, AbiMismatch, AbiStructure};
use crate::bytecode::{
    linked_opcode_table, opcode, CompileSnapshot, DeoptPoint, HelperId, OsrPoint, RuntimeConstants,
    SnapshotStatus, VerifiedFunction, VerifierMetadata, VerifyLimits,
};

#[cfg(feature = "compiler")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct JitTraceEvent {
    pc: u32,
    opcode: u8,
    kind: u8,
    helper_id: u8,
    reserved: u8,
}

#[cfg(feature = "compiler")]
unsafe extern "C" {
    fn JS_JitSetExecutionTrace(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        events: *mut JitTraceEvent,
        capacity: u32,
    ) -> i32;
    fn JS_JitGetExecutionTraceLength(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        length: *mut u32,
        overflowed: *mut u32,
    ) -> i32;
}
use crate::code_cache::CompiledArtifact;
#[cfg(feature = "compiler")]
use crate::compiler::baseline::{BaselineCompiler, PublishedBaselineCode};
use crate::compiler::{mock::FakeCompiler, mock::FakeCompilerControl, Compiler};
use crate::runtime::{
    CompileCompletion, CompileFailure, CompileRequest, CompileState, Coordinator, FunctionKey, Tier,
};
use crate::{Jit, JitConfig, JitDiagnosticKind, JitError, JitMetrics};

pub use crate::bytecode::decode_raw;

fn coordinator_snapshot() -> VerifiedFunction {
    CompileSnapshot::from_untrusted_bytecode(vec![opcode::RETURN_UNDEF], 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .expect("coordinator fixture verifies")
}

pub struct Harness {
    coordinator: Mutex<Coordinator>,
    requests: Mutex<HashMap<FunctionKey, CompileRequest>>,
    compiler: FakeCompiler,
    control: FakeCompilerControl,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    pub fn new() -> Self {
        Self::with_max_attempts(4)
    }

    pub fn with_max_attempts(max_attempts: u8) -> Self {
        let (compiler, control) = FakeCompiler::new(8);
        Self {
            coordinator: Mutex::new(Coordinator::with_limits(8, 8, max_attempts, 3)),
            requests: Mutex::new(HashMap::new()),
            compiler,
            control,
        }
    }

    pub fn queue(&self, key: FunctionKey) {
        let request = {
            let mut coordinator = self
                .coordinator
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            coordinator
                .queue(key, Tier::Baseline, coordinator_snapshot())
                .expect("harness queue accepted");
            coordinator.begin_next().expect("harness request begins")
        };
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, request);
    }

    pub fn retire(&self, key: FunctionKey) {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retire(key);
    }

    pub fn complete(&self, key: FunctionKey, artifact: CompiledArtifact) {
        let request = self
            .requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key)
            .expect("harness request exists");
        let requested_tier = request.tier();
        let artifact_key = request.artifact_key();
        let attempt_id = request.attempt_id();
        self.control.complete(artifact);
        let result = self.compiler.compile(request);
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .complete(CompileCompletion {
                key,
                requested_tier,
                artifact_key,
                attempt_id,
                result,
            });
    }

    pub fn fail(&self, key: FunctionKey, failure: CompileFailure) {
        let request = {
            let mut coordinator = self
                .coordinator
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let CompileState::Backoff { retry_after, .. } = coordinator.state(key) {
                coordinator.advance_clock(retry_after);
            }
            coordinator
                .queue(key, Tier::Baseline, coordinator_snapshot())
                .expect("harness failure queued");
            coordinator.begin_next().expect("harness failure begins")
        };
        let requested_tier = request.tier();
        let artifact_key = request.artifact_key();
        let attempt_id = request.attempt_id();
        self.control.fail(failure);
        let result = self.compiler.compile(request);
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .complete(CompileCompletion {
                key,
                requested_tier,
                artifact_key,
                attempt_id,
                result,
            });
    }

    pub fn state(&self, key: FunctionKey) -> CompileState {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state(key)
    }

    pub fn metrics(&self) -> JitMetrics {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .metrics()
            .clone()
    }
}

pub struct SnapshotFixture {
    runtime: Runtime,
    _context: Context,
    snapshot: CompileSnapshot,
    _runtime_constants: RuntimeConstants,
}

impl SnapshotFixture {
    pub fn compile(source: &str) -> Self {
        let runtime = Runtime::new().expect("snapshot runtime");
        let context = Context::full(&runtime).expect("snapshot context");
        let snapshot = context.with(|ctx| {
            let mut options = EvalOptions::default();
            options.global = true;
            options.strict = false;
            let function: Value<'_> = ctx
                .eval_with_options(source, options)
                .expect("compile snapshot fixture");
            unsafe {
                CompileSnapshot::capture_with_runtime_constants(
                    &runtime,
                    ctx.as_raw().as_ptr(),
                    function.as_raw(),
                )
            }
            .expect("snapshot supported function")
        });
        let (snapshot, runtime_constants) = snapshot;
        Self {
            runtime,
            _context: context,
            snapshot,
            _runtime_constants: runtime_constants,
        }
    }

    pub fn snapshot(&self) -> CompileSnapshot {
        self.snapshot.clone()
    }
}

impl Drop for SnapshotFixture {
    fn drop(&mut self) {
        self.runtime.run_gc();
    }
}

#[cfg(feature = "compiler")]
struct ForcedBaselineBackend {
    function_id: u64,
    generation: u64,
    code: PublishedBaselineCode,
    entries: Arc<AtomicUsize>,
    retries: Arc<AtomicUsize>,
    deopts: Arc<AtomicUsize>,
    registry: Arc<Mutex<Option<JitFunctionRegistry>>>,
    stress_gc: bool,
}

#[cfg(feature = "compiler")]
struct ForcedEntryPin {
    code: PublishedBaselineCode,
    entries: Arc<AtomicUsize>,
    retries: Arc<AtomicUsize>,
    deopts: Arc<AtomicUsize>,
    stress_gc: bool,
}

#[cfg(all(feature = "compiler", rquickjs_memory_sanitizer))]
unsafe fn unpoison_native_outcome(
    exit: *const rquickjs_core::qjs::JSJitExit,
    frame: *const rquickjs_core::qjs::JSJitExecFrame,
) {
    unsafe extern "C" {
        fn __msan_unpoison(address: *const core::ffi::c_void, size: usize);
    }

    unsafe {
        if !exit.is_null() {
            __msan_unpoison(exit.cast(), mem::size_of::<rquickjs_core::qjs::JSJitExit>());
        }
        __msan_unpoison(
            frame.cast(),
            mem::size_of::<rquickjs_core::qjs::JSJitExecFrame>(),
        );
    }
}

#[cfg(all(feature = "compiler", rquickjs_memory_sanitizer))]
unsafe fn unpoison_quickjs_frame_values(frame: *const rquickjs_core::qjs::JSJitExecFrame) {
    unsafe extern "C" {
        fn __msan_unpoison(address: *const core::ffi::c_void, size: usize);
    }

    unsafe {
        let values_start = (*frame).arg_buf.cast::<u8>();
        let values_end = (*frame).stack_capacity.cast::<u8>();
        if !values_start.is_null() {
            if let Some(values_size) = (values_end as usize).checked_sub(values_start as usize) {
                if values_size <= 64 * 1024 * 1024 {
                    __msan_unpoison(values_start.cast(), values_size);
                }
            }
        }
        let locals_start = (*frame).var_buf.cast::<u8>();
        if !locals_start.is_null() {
            if let Some(locals_size) = (values_end as usize).checked_sub(locals_start as usize) {
                if locals_size <= 64 * 1024 * 1024 {
                    __msan_unpoison(locals_start.cast(), locals_size);
                }
            }
        }
    }
}

#[cfg(feature = "compiler")]
unsafe extern "C" fn forced_entry_trampoline(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
) -> rquickjs_core::qjs::JSJitExit {
    type Entry = unsafe extern "C" fn(
        *mut rquickjs_core::qjs::JSJitExecFrame,
    ) -> rquickjs_core::qjs::JSJitExit;
    let pin = unsafe { &*((*frame).entry.pin.cast::<ForcedEntryPin>()) };
    if pin.stress_gc {
        unsafe { (*frame).flags |= rquickjs_core::qjs::JS_JIT_FRAME_STRESS_GC };
    }
    pin.entries.fetch_add(1, Ordering::SeqCst);
    let native: Entry = unsafe { mem::transmute(pin.code.as_ptr()) };
    #[cfg(rquickjs_memory_sanitizer)]
    unsafe {
        unpoison_native_outcome(ptr::null(), frame);
        unpoison_quickjs_frame_values(frame);
    }
    let exit = unsafe { native(frame) };
    #[cfg(rquickjs_memory_sanitizer)]
    unsafe {
        unpoison_native_outcome(ptr::addr_of!(exit), frame);
    }
    if exit.kind == rquickjs_core::qjs::JSJitExitKind_JS_JIT_EXIT_RETRY_INTERPRETER {
        pin.retries.fetch_add(1, Ordering::SeqCst);
    }
    if exit.kind == rquickjs_core::qjs::JSJitExitKind_JS_JIT_EXIT_DEOPT {
        pin.deopts.fetch_add(1, Ordering::SeqCst);
    }
    exit
}

#[cfg(feature = "compiler")]
unsafe impl JitBackend for ForcedBaselineBackend {
    fn runtime_attached(&mut self, registry: JitFunctionRegistry) {
        *self
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(registry);
    }

    fn acquire_entry(
        &mut self,
        id: u64,
        generation: u64,
        pc: u32,
    ) -> rquickjs_core::qjs::JSJitEntryHandle {
        if id != self.function_id || generation != self.generation || pc != 0 {
            return empty_entry_handle();
        }
        rquickjs_core::qjs::JSJitEntryHandle {
            struct_size: mem::size_of::<rquickjs_core::qjs::JSJitEntryHandle>() as u32,
            reserved: 0,
            entry: Some(forced_entry_trampoline),
            pin: Box::into_raw(Box::new(ForcedEntryPin {
                code: self.code.clone(),
                entries: Arc::clone(&self.entries),
                retries: Arc::clone(&self.retries),
                deopts: Arc::clone(&self.deopts),
                stress_gc: self.stress_gc,
            }))
            .cast(),
            // Stack-map ids are frame-state source locations; states Cranelift
            // removed leave gaps, so the count is the production bound.
            stack_map_count: self.code.required_stack_map_count(),
            helper_abi_version: rquickjs_core::qjs::QJSJIT_HELPER_ABI_VERSION,
        }
    }

    fn release_entry(&mut self, entry: rquickjs_core::qjs::JSJitEntryHandle) {
        if !entry.pin.is_null() {
            unsafe { drop(Box::from_raw(entry.pin.cast::<ForcedEntryPin>())) };
        }
    }

    fn memory_used(&self) -> usize {
        self.code.len()
    }
}

#[cfg(feature = "compiler")]
fn empty_entry_handle() -> rquickjs_core::qjs::JSJitEntryHandle {
    rquickjs_core::qjs::JSJitEntryHandle {
        struct_size: mem::size_of::<rquickjs_core::qjs::JSJitEntryHandle>() as u32,
        reserved: 0,
        entry: None,
        pin: ptr::null_mut(),
        stack_map_count: 0,
        helper_abi_version: 0,
    }
}

#[cfg(feature = "compiler")]
fn eval_global_definition(context: &Context, definition: &str) -> Result<(), String> {
    context.with(|ctx| {
        let mut options = EvalOptions::default();
        options.global = true;
        options.strict = false;
        ctx.eval_with_options::<(), _>(definition, options)
            .map_err(|error| format!("{error:?}"))
    })
}

#[cfg(feature = "compiler")]
fn install_canonical_observer(context: &Context) {
    context.with(|ctx| {
        ctx.eval::<(), _>(crate::correctness::canonical_observer_prelude())
            .expect("install trusted canonical observer before untrusted source")
    });
}

#[cfg(feature = "compiler")]
fn eval_canonical(context: &Context, expression: &str) -> Result<String, String> {
    let source = crate::correctness::canonical_observation_call_source(expression);
    context.with(|ctx| {
        ctx.eval::<String, _>(source)
            .map_err(|error| format!("{error:?}"))
    })
}

#[cfg(feature = "compiler")]
struct ForcedInstallation {
    guard: RuntimeJitGuard,
    entries: Arc<AtomicUsize>,
    retries: Arc<AtomicUsize>,
    deopts: Arc<AtomicUsize>,
    registry: Arc<Mutex<Option<JitFunctionRegistry>>>,
}

#[cfg(feature = "compiler")]
fn compile_named_function(
    runtime: &Runtime,
    context: &Context,
    definition: &str,
    function_name: &str,
    stress_gc: bool,
) -> ForcedInstallation {
    eval_global_definition(context, definition)
        .unwrap_or_else(|error| panic!("baseline definition failed: {error}"));
    let snapshot = context.with(|ctx| {
        let function: Function<'_> = ctx
            .globals()
            .get(function_name)
            .unwrap_or_else(|error| panic!("missing baseline function {function_name}: {error:?}"));
        unsafe { CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_value().as_raw()) }
            .unwrap_or_else(|status| panic!("baseline snapshot failed: {status:?}"))
    });
    let verified = snapshot
        .clone()
        .verify(VerifyLimits::default())
        .unwrap_or_else(|error| panic!("baseline verification failed: {error:?}"));
    let code = crate::ir::with_execution_trace(|| BaselineCompiler::host().compile(&verified))
        .unwrap_or_else(|error| {
            panic!(
                "forced baseline compilation failed: {error:?}; bytecode={:?}",
                snapshot.decode()
            )
        })
        .publish()
        .unwrap_or_else(|error| panic!("forced baseline publication failed: {error:?}"));
    let entries = Arc::new(AtomicUsize::new(0));
    let retries = Arc::new(AtomicUsize::new(0));
    let deopts = Arc::new(AtomicUsize::new(0));
    let registry_slot = Arc::new(Mutex::new(None));
    let guard = runtime
        .attach_jit_backend(ForcedBaselineBackend {
            function_id: snapshot.function_id(),
            generation: snapshot.generation(),
            code,
            entries: Arc::clone(&entries),
            retries: Arc::clone(&retries),
            deopts: Arc::clone(&deopts),
            registry: Arc::clone(&registry_slot),
            stress_gc,
        })
        .expect("forced baseline backend attaches");
    let registry = registry_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("forced baseline registry is delivered during attach");
    context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get(function_name).unwrap();
        registry
            .retain_function(
                &ctx,
                &function,
                snapshot.function_id(),
                snapshot.generation(),
            )
            .expect("forced baseline function retention");
        assert_eq!(registry.retained_len(&ctx).unwrap(), 1);
    });
    ForcedInstallation {
        guard,
        entries,
        retries,
        deopts,
        registry: registry_slot,
    }
}

#[cfg(feature = "compiler")]
#[derive(Clone, Debug)]
pub struct DifferentialRun {
    definition: String,
    expression: String,
    require_baseline: bool,
    expected_opcode: Option<String>,
    expected_helper: Option<HelperId>,
    expected_ownership_helper_counts: Option<(u64, u64)>,
    expected_helper_count: Option<(u32, u64)>,
    stress_gc: bool,
    expect_deopt: bool,
}

#[cfg(feature = "compiler")]
pub fn differential(definition: &str, expression: &str) -> DifferentialRun {
    DifferentialRun {
        definition: definition.to_owned(),
        expression: expression.to_owned(),
        require_baseline: true,
        expected_opcode: None,
        expected_helper: None,
        expected_ownership_helper_counts: None,
        expected_helper_count: None,
        stress_gc: false,
        expect_deopt: false,
    }
}

#[cfg(feature = "compiler")]
pub fn assert_tier1_rejected(
    definition: &str,
    expression: &str,
    expected: crate::bytecode::FallbackReason,
) {
    let runtime = Runtime::new().expect("rejected fixture runtime");
    let context = Context::full(&runtime).expect("rejected fixture context");
    install_canonical_observer(&context);
    eval_global_definition(&context, definition).expect("rejected fixture definition");
    let expected_value = eval_canonical(&context, expression);
    let snapshot = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("f").expect("rejected fixture f");
        unsafe { CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_value().as_raw()) }
            .expect("rejected fixture snapshot")
    });
    let verified = snapshot
        .verify(VerifyLimits::default())
        .expect("rejected fixture verifies");
    let rejection = verified
        .tier1_eligibility()
        .expect_err("fixture must reject Tier 1");
    assert_eq!(rejection.reason(), expected);
    assert!(matches!(
        BaselineCompiler::host().compile(&verified),
        Err(crate::compiler::CompileFailure::Tier1Rejected(reason)) if reason == expected
    ));
    assert_eq!(eval_canonical(&context, expression), expected_value);
}

#[cfg(feature = "compiler")]
impl DifferentialRun {
    pub fn force_baseline(mut self) -> Self {
        self.require_baseline = true;
        self
    }

    pub fn expect_executed_opcode(mut self, opcode: &str) -> Self {
        self.expected_opcode = Some(opcode.to_owned());
        self
    }

    pub fn expect_helper(mut self, helper: HelperId) -> Self {
        self.expected_helper = Some(helper);
        self
    }

    pub fn expect_ownership_helper_counts(mut self, dup: u64, free: u64) -> Self {
        self.expected_ownership_helper_counts = Some((dup, free));
        self
    }

    pub fn stress_gc(mut self) -> Self {
        self.stress_gc = true;
        self
    }

    /// Exact number of calls of one runtime helper (by generated helper ID)
    /// while evaluating the expression.
    pub fn expect_helper_call_count(mut self, helper_id: u32, count: u64) -> Self {
        self.expected_helper_count = Some((helper_id, count));
        self
    }

    /// The native body is expected to hand the frame back to the
    /// interpreter mid-function (a use-site guard failed) at least once.
    pub fn expect_deopt(mut self) -> Self {
        self.expect_deopt = true;
        self
    }

    pub fn assert_same(self) {
        let interpreter_runtime = Runtime::new().expect("interpreter runtime");
        let interpreter_context = Context::full(&interpreter_runtime).expect("interpreter context");
        install_canonical_observer(&interpreter_context);
        eval_global_definition(&interpreter_context, &self.definition)
            .unwrap_or_else(|error| panic!("interpreter definition failed: {error}"));
        let expected = eval_canonical(&interpreter_context, &self.expression);

        let runtime = Runtime::new().expect("compiled runtime");
        let context = Context::full(&runtime).expect("compiled context");
        install_canonical_observer(&context);
        let installation =
            compile_named_function(&runtime, &context, &self.definition, "f", self.stress_gc);
        let rt =
            context.with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
        let mut trace = vec![JitTraceEvent::default(); 16_384];
        assert_eq!(
            unsafe { rquickjs_core::qjs::JS_JitResetHelperCounters(rt) },
            0
        );
        assert_eq!(
            unsafe { JS_JitSetExecutionTrace(rt, trace.as_mut_ptr(), trace.len() as u32) },
            0
        );
        let actual = eval_canonical(&context, &self.expression);

        if let Some((expected_dup, expected_free)) = self.expected_ownership_helper_counts {
            let mut counters = rquickjs_core::qjs::JSJitHelperCounters {
                struct_size: std::mem::size_of::<rquickjs_core::qjs::JSJitHelperCounters>() as u32,
                reserved: 0,
                dup_count: 0,
                free_count: 0,
            };
            assert_eq!(
                unsafe { rquickjs_core::qjs::JS_JitGetHelperCounters(rt, &mut counters) },
                0
            );
            assert_eq!(
                (counters.dup_count, counters.free_count),
                (expected_dup, expected_free)
            );
        }

        if let Some((helper_id, expected_count)) = self.expected_helper_count {
            let mut count = 0_u64;
            assert_eq!(
                unsafe { rquickjs_core::qjs::JS_JitGetHelperCount(rt, helper_id, &mut count) },
                0
            );
            assert_eq!(count, expected_count, "helper {helper_id} call count");
        }

        let mut trace_len = 0;
        let mut overflowed = 0;
        assert_eq!(
            unsafe { JS_JitGetExecutionTraceLength(rt, &mut trace_len, &mut overflowed) },
            0
        );
        assert_eq!(overflowed, 0, "native execution trace overflowed");
        trace.truncate(trace_len as usize);
        assert_eq!(
            unsafe { JS_JitSetExecutionTrace(rt, ptr::null_mut(), 0) },
            0
        );
        if std::env::var_os("QJSJIT_DUMP_TRACE").is_some() {
            eprintln!(
                "QJSJIT_TRACE {}",
                trace
                    .iter()
                    .filter_map(
                        |event| linked_opcode_table().find(|opcode| opcode.id() == event.opcode)
                    )
                    .map(|opcode| format!(
                        "{}@{}",
                        opcode.name(),
                        trace
                            .iter()
                            .find(|event| event.opcode == opcode.id())
                            .map_or(0, |event| event.pc)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }

        assert_eq!(actual, expected, "forced baseline changed JS semantics");
        if self.require_baseline {
            assert!(
                installation.entries.load(Ordering::SeqCst) > 0,
                "forced baseline expression never entered published native code"
            );
            assert_eq!(
                installation.retries.load(Ordering::SeqCst),
                0,
                "forced baseline silently retried in the interpreter"
            );
            let deopts = installation.deopts.load(Ordering::SeqCst);
            if self.expect_deopt {
                assert!(
                    deopts > 0,
                    "forced baseline never deoptimized to the interpreter"
                );
            } else {
                assert_eq!(
                    deopts, 0,
                    "forced baseline silently deoptimized to the interpreter"
                );
            }
        }
        if let Some(expected) = self.expected_opcode.as_deref() {
            let opcode = linked_opcode_table()
                .find(|opcode| opcode.name() == expected)
                .unwrap_or_else(|| panic!("unknown expected opcode {expected}"));
            assert!(
                trace.iter().any(|event| event.opcode == opcode.id()),
                "native body did not execute target opcode {expected}; trace={:?}",
                trace
                    .iter()
                    .map(|event| (event.pc, event.opcode))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(helper) = self.expected_helper {
            let helper_id = match helper {
                HelperId::Dup => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_DUP,
                HelperId::Free => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_FREE,
                HelperId::ResolveConst => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_RESOLVE_CONST
                }
                HelperId::AtomValue => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_ATOM_VALUE,
                HelperId::ToNumeric => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_TO_NUMERIC,
                HelperId::ToBool => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_TO_BOOL,
                HelperId::AddSlow => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_ADD_SLOW,
                HelperId::BinaryArithSlow => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_BINARY_ARITH_SLOW
                }
                HelperId::UnaryArithSlow => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_UNARY_ARITH_SLOW
                }
                HelperId::CompareSlow => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_COMPARE_SLOW
                }
                HelperId::GetProperty => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY
                }
                HelperId::SetProperty => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_PROPERTY
                }
                HelperId::Call => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL,
                HelperId::CallConstructor => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL_CONSTRUCTOR
                }
                HelperId::Regexp => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_REGEXP,
                HelperId::NewArray => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_NEW_ARRAY,
                HelperId::NewObject => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_NEW_OBJECT,
                HelperId::GetElement => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_ELEMENT,
                HelperId::SetElement => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_ELEMENT,
                HelperId::ToPropertyKey => {
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_TO_PROPKEY
                }
                HelperId::GetGlobal => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_GLOBAL,
                HelperId::FClosure => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_FCLOSURE,
                HelperId::GetVarRef => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_VAR_REF,
                HelperId::PutVarRef => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_PUT_VAR_REF,
                HelperId::CloseLocal => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CLOSE_LOC,
                HelperId::SetName => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_NAME,
                HelperId::GenericOp => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GENERIC_OP,
                HelperId::IteratorOp => rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_ITERATOR_OP,
            };
            let expected_opcode = self
                .expected_opcode
                .as_deref()
                .expect("helper evidence requires an expected opcode");
            let opcode = linked_opcode_table()
                .find(|opcode| opcode.name() == expected_opcode)
                .expect("expected helper opcode is linked");
            assert!(
                trace.iter().any(|event| {
                    event.kind == 1
                        && u32::from(event.helper_id) == helper_id
                        && event.opcode == opcode.id()
                        && trace.iter().any(|opcode_event| {
                            opcode_event.kind == 0
                                && opcode_event.pc == event.pc
                                && opcode_event.opcode == event.opcode
                        })
                }),
                "expected helper {helper:?} was not executed by target opcode {expected_opcode}; trace={:?}",
                trace
                    .iter()
                    .map(|event| (event.pc, event.opcode, event.kind, event.helper_id))
                    .collect::<Vec<_>>()
            );
        }
        drop(installation.guard);
        assert!(
            installation
                .registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .is_some_and(|registry| !registry.is_attached()),
            "runtime detachment left the function registry attached"
        );
    }
}

#[cfg(feature = "compiler")]
#[derive(Clone, Debug)]
pub struct ForcedBaselineRun {
    definition: String,
    expression: String,
    interrupt_after: usize,
}

#[cfg(feature = "compiler")]
pub fn forced_baseline(source: &str) -> ForcedBaselineRun {
    let split = source
        .rfind(" f(")
        .unwrap_or_else(|| panic!("forced_baseline source must end in an f(...) expression"));
    ForcedBaselineRun {
        definition: source[..split].trim().to_owned(),
        expression: source[(split + 1)..].trim().to_owned(),
        interrupt_after: 1,
    }
}

#[cfg(feature = "compiler")]
impl ForcedBaselineRun {
    pub fn interrupt_after(mut self, polls: usize) -> Self {
        self.interrupt_after = polls.max(1);
        self
    }

    pub fn assert_uncatchable_interrupt(self) {
        let runtime = Runtime::new().expect("compiled interrupt runtime");
        let context = Context::full(&runtime).expect("compiled interrupt context");
        let installation = compile_named_function(&runtime, &context, &self.definition, "f", false);
        let polls = Arc::new(AtomicUsize::new(0));
        runtime.set_interrupt_handler({
            let polls = Arc::clone(&polls);
            let interrupt_after = self.interrupt_after;
            Some(Box::new(move || {
                polls.fetch_add(1, Ordering::SeqCst) + 1 >= interrupt_after
            }))
        });
        let source = format!(
            "try {{ {}; 'caught' }} catch (_) {{ 'caught' }}",
            self.expression
        );
        let result = context.with(|ctx| ctx.eval::<String, _>(source));

        assert!(result.is_err(), "compiled interrupt became catchable");
        assert!(
            installation.entries.load(Ordering::SeqCst) > 0,
            "interrupt test never entered published native code"
        );
        assert!(
            polls.load(Ordering::SeqCst) >= self.interrupt_after,
            "compiled loop did not execute the requested poll budget"
        );
        drop(installation.guard);
        assert!(
            installation
                .registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .is_some_and(|registry| !registry.is_attached()),
            "interrupt teardown left the function registry attached"
        );
    }
}

pub fn snapshot_status(source: &str) -> SnapshotStatus {
    let runtime = Runtime::new().expect("snapshot runtime");
    let context = Context::full(&runtime).expect("snapshot context");
    context.with(|ctx| {
        let mut options = EvalOptions::default();
        options.global = true;
        options.strict = false;
        let function: Value<'_> = ctx
            .eval_with_options(source, options)
            .expect("compile snapshot status fixture");
        match unsafe { CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_raw()) } {
            Ok(_) => SnapshotStatus::Ok,
            Err(status) => status,
        }
    })
}

pub fn snapshot_from_parts(
    bytecode: Vec<u8>,
    arg_count: u16,
    local_count: u16,
    closure_count: u16,
    constant_count: u32,
) -> CompileSnapshot {
    CompileSnapshot::from_untrusted_bytecode(
        bytecode,
        arg_count,
        local_count,
        closure_count,
        constant_count,
    )
}

/// Builds the smallest worker-safe verified function used by compiler tests.
pub fn verified_bytecode(bytecode: Vec<u8>, arg_count: u16, local_count: u16) -> VerifiedFunction {
    CompileSnapshot::from_untrusted_bytecode(bytecode, arg_count, local_count, 0, 0)
        .verify(VerifyLimits::default())
        .expect("synthetic bytecode verifies")
}

/// Compiles a synthetic fixture through implemented lowering arms without
/// changing the policy applied by the public compiler entry point.
#[cfg(feature = "compiler")]
#[doc(hidden)]
pub fn compile_implemented_fixture(
    compiler: &BaselineCompiler,
    function: &VerifiedFunction,
) -> Result<crate::compiler::baseline::RelocatableCode, CompileFailure> {
    compiler.compile_implemented_for_test(function)
}

/// Stable Rust representation of QuickJS's 16-byte non-NaN-boxed value ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JSValueRepr {
    pub payload: u64,
    pub tag: i64,
}

impl JSValueRepr {
    pub const fn new(payload: u64, tag: i64) -> Self {
        Self { payload, tag }
    }

    pub const fn undefined() -> Self {
        Self::new(0, rquickjs_core::qjs::JS_TAG_UNDEFINED as i64)
    }

    pub const fn int32(value: i32) -> Self {
        Self::new(value as i64 as u64, rquickjs_core::qjs::JS_TAG_INT as i64)
    }

    pub const fn float64(value: f64) -> Self {
        Self::new(value.to_bits(), rquickjs_core::qjs::JS_TAG_FLOAT64 as i64)
    }

    pub fn as_f64(self) -> Option<f64> {
        (self.tag == rquickjs_core::qjs::JS_TAG_FLOAT64 as i64)
            .then(|| f64::from_bits(self.payload))
    }

    fn into_raw(self) -> rquickjs_core::qjs::JSValue {
        // The ABI validation tests independently prove both layouts are 16
        // bytes with the tag at byte 8.
        unsafe { mem::transmute(self) }
    }

    pub fn from_raw(value: rquickjs_core::qjs::JSValue) -> Self {
        unsafe { mem::transmute(value) }
    }
}

const _: () =
    assert!(mem::size_of::<JSValueRepr>() == mem::size_of::<rquickjs_core::qjs::JSValue>());
const _: () =
    assert!(mem::offset_of!(JSValueRepr, tag) == mem::offset_of!(rquickjs_core::qjs::JSValue, tag));

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AlignedStackSlot(JSValueRepr);

const _: () = assert!(mem::size_of::<AlignedStackSlot>() == 16);
const _: () = assert!(mem::align_of::<AlignedStackSlot>() == 16);

#[derive(Debug)]
struct PollState {
    count: AtomicUsize,
    interrupt_at: AtomicUsize,
    capture_backtrace: AtomicBool,
    backtrace: Mutex<Vec<usize>>,
    argument_count: usize,
    local_count: usize,
}

unsafe fn synthetic_slot(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    slot: u32,
) -> Option<*mut JSValueRepr> {
    let frame = unsafe { &mut *frame };
    let state = unsafe { &*(frame.rt.cast::<PollState>()) };
    let slot = usize::try_from(slot).ok()?;
    if slot < state.argument_count {
        return Some(unsafe { frame.arg_buf.cast::<JSValueRepr>().add(slot) });
    }
    let local = slot - state.argument_count;
    if local < state.local_count {
        return Some(unsafe { frame.var_buf.cast::<JSValueRepr>().add(local) });
    }
    let stack = local - state.local_count;
    let live = unsafe { frame.stack_top.offset_from(frame.stack_base) };
    let live = usize::try_from(live).ok()?;
    (stack < live).then(|| unsafe { frame.stack_base.cast::<JSValueRepr>().add(stack) })
}

unsafe extern "C" fn synthetic_dup(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    output: u32,
    input: u32,
) -> i32 {
    let (Some(output), Some(input)) = (unsafe { synthetic_slot(frame, output) }, unsafe {
        synthetic_slot(frame, input)
    }) else {
        return -1;
    };
    if output == input || unsafe { *output } != JSValueRepr::undefined() {
        return -1;
    }
    unsafe { *output = *input };
    0
}

unsafe extern "C" fn synthetic_free(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    input: u32,
) -> i32 {
    let Some(input) = (unsafe { synthetic_slot(frame, input) }) else {
        return -1;
    };
    unsafe { *input = JSValueRepr::undefined() };
    0
}

fn synthetic_number(value: JSValueRepr) -> Option<f64> {
    match value.tag as i32 {
        rquickjs_core::qjs::JS_TAG_INT => Some(value.payload as i64 as i32 as f64),
        rquickjs_core::qjs::JS_TAG_FLOAT64 => Some(f64::from_bits(value.payload)),
        _ => None,
    }
}

unsafe extern "C" fn synthetic_to_numeric(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    output: u32,
    input: u32,
) -> i32 {
    let (Some(output), Some(input)) = (unsafe { synthetic_slot(frame, output) }, unsafe {
        synthetic_slot(frame, input)
    }) else {
        return -1;
    };
    let value = unsafe { *input };
    let result = match value.tag as i32 {
        rquickjs_core::qjs::JS_TAG_INT | rquickjs_core::qjs::JS_TAG_FLOAT64 => value,
        rquickjs_core::qjs::JS_TAG_BOOL => JSValueRepr::int32(value.payload as i32),
        rquickjs_core::qjs::JS_TAG_NULL => JSValueRepr::int32(0),
        rquickjs_core::qjs::JS_TAG_UNDEFINED => JSValueRepr::float64(f64::NAN),
        _ => {
            unsafe { *input = JSValueRepr::undefined() };
            return -1;
        }
    };
    unsafe {
        *input = JSValueRepr::undefined();
        *output = result;
    }
    0
}

unsafe extern "C" fn synthetic_to_bool(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    output: u32,
    input: u32,
) -> i32 {
    let (Some(output), Some(input)) = (unsafe { synthetic_slot(frame, output) }, unsafe {
        synthetic_slot(frame, input)
    }) else {
        return -1;
    };
    let value = unsafe { *input };
    let truthy = match value.tag as i32 {
        rquickjs_core::qjs::JS_TAG_UNDEFINED | rquickjs_core::qjs::JS_TAG_NULL => false,
        rquickjs_core::qjs::JS_TAG_FLOAT64 => {
            let number = f64::from_bits(value.payload);
            number != 0.0 && !number.is_nan()
        }
        rquickjs_core::qjs::JS_TAG_INT
        | rquickjs_core::qjs::JS_TAG_BOOL
        | rquickjs_core::qjs::JS_TAG_SHORT_BIG_INT => value.payload != 0,
        _ => true,
    };
    unsafe {
        *input = JSValueRepr::undefined();
        *output = JSValueRepr::new(truthy as u64, rquickjs_core::qjs::JS_TAG_BOOL as i64);
    }
    0
}

unsafe extern "C" fn synthetic_add(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    output: u32,
    left: u32,
    right: u32,
) -> i32 {
    let (Some(output), Some(left), Some(right)) = (
        unsafe { synthetic_slot(frame, output) },
        unsafe { synthetic_slot(frame, left) },
        unsafe { synthetic_slot(frame, right) },
    ) else {
        return -1;
    };
    let lhs = unsafe { *left };
    let rhs = unsafe { *right };
    let result = if lhs.tag == rquickjs_core::qjs::JS_TAG_INT as i64
        && rhs.tag == rquickjs_core::qjs::JS_TAG_INT as i64
    {
        let lhs = lhs.payload as i64 as i32;
        let rhs = rhs.payload as i64 as i32;
        match lhs.checked_add(rhs) {
            Some(value) => JSValueRepr::int32(value),
            None => JSValueRepr::float64(f64::from(lhs) + f64::from(rhs)),
        }
    } else if let (Some(lhs), Some(rhs)) = (synthetic_number(lhs), synthetic_number(rhs)) {
        JSValueRepr::float64(lhs + rhs)
    } else {
        unsafe {
            *left = JSValueRepr::undefined();
            *right = JSValueRepr::undefined();
        }
        return -1;
    };
    unsafe {
        *right = JSValueRepr::undefined();
        *output = result;
    }
    0
}

unsafe extern "C" fn synthetic_compare(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    output: u32,
    left: u32,
    right: u32,
    operation: u32,
) -> i32 {
    let (Some(output), Some(left), Some(right)) = (
        unsafe { synthetic_slot(frame, output) },
        unsafe { synthetic_slot(frame, left) },
        unsafe { synthetic_slot(frame, right) },
    ) else {
        return -1;
    };
    let lhs = unsafe { *left };
    let rhs = unsafe { *right };
    let Some(lhs_number) = synthetic_number(lhs) else {
        return -1;
    };
    let Some(rhs_number) = synthetic_number(rhs) else {
        return -1;
    };
    let result = match operation {
        rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_LT => lhs_number < rhs_number,
        rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_LTE => lhs_number <= rhs_number,
        rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_GT => lhs_number > rhs_number,
        rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_GTE => lhs_number >= rhs_number,
        rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_EQ
        | rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_STRICT_EQ => lhs_number == rhs_number,
        rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_NEQ
        | rquickjs_core::qjs::JSJitCompareOp_JS_JIT_COMPARE_STRICT_NEQ => lhs_number != rhs_number,
        _ => return -1,
    };
    unsafe {
        *right = JSValueRepr::undefined();
        *output = JSValueRepr::new(result as u64, rquickjs_core::qjs::JS_TAG_BOOL as i64);
    }
    0
}

unsafe extern "C" fn synthetic_map_in_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _input: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_map_out_in_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
    _input: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_map_call_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
    _function: u32,
    _new_target: u32,
    _argv: u32,
    _argc: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_map_out_two_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
    _left: u32,
    _right: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_map_out_two_op_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
    _left: u32,
    _right: u32,
    _opcode: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_map_out_in_op_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
    _input: u32,
    _opcode: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_get_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
    _object: u32,
    _atom: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_set_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _object: u32,
    _atom: u32,
    _value: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_call_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
    _function: u32,
    _this_value: u32,
    _argv: u32,
    _argc: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_new_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output: u32,
) -> i32 {
    -1
}

unsafe extern "C" fn synthetic_shape_guard_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _object: u32,
    _identity_lo: u32,
    _identity_hi: u32,
    _generation_lo: u32,
    _generation_hi: u32,
) -> i32 {
    rquickjs_core::qjs::JS_JIT_HELPER_GUARD_MISS
}

unsafe extern "C" fn synthetic_materialize_owner_unavailable(
    _frame: *mut rquickjs_core::qjs::JSJitExecFrame,
    _stack_map_id: u32,
    _output_stack_index: u32,
    _source_kind: u32,
    _source_index: u32,
) -> i32 {
    rquickjs_core::qjs::JS_JIT_HELPER_EXCEPTION
}

unsafe extern "C" fn synthetic_interrupt_poll(
    frame: *mut rquickjs_core::qjs::JSJitExecFrame,
) -> i32 {
    let state = unsafe { &*((*frame).rt.cast::<PollState>()) };
    let count = state.count.fetch_add(1, Ordering::AcqRel) + 1;
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if state.capture_backtrace.swap(false, Ordering::AcqRel) {
        let mut frames = [ptr::null_mut(); 64];
        let frame_count = unsafe { libc::backtrace(frames.as_mut_ptr(), frames.len() as i32) };
        let frame_count = usize::try_from(frame_count.max(0)).unwrap_or(0);
        let mut backtrace = state
            .backtrace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        backtrace.clear();
        backtrace.extend(
            frames[..frame_count]
                .iter()
                .map(|address| *address as usize),
        );
    }
    usize::from(count == state.interrupt_at.load(Ordering::Acquire)) as i32
}

static SYNTHETIC_RUNTIME_API: rquickjs_core::qjs::JSJitRuntimeAPI =
    rquickjs_core::qjs::JSJitRuntimeAPI {
        struct_size: mem::size_of::<rquickjs_core::qjs::JSJitRuntimeAPI>() as u32,
        major: rquickjs_core::qjs::QJSJIT_RUNTIME_API_MAJOR as u16,
        minor: rquickjs_core::qjs::QJSJIT_RUNTIME_API_MINOR as u16,
        interrupt_poll: Some(synthetic_interrupt_poll),
        dup: Some(synthetic_dup),
        free: Some(synthetic_free),
        resolve_const: Some(synthetic_map_out_in_unavailable),
        atom_value: Some(synthetic_map_out_in_unavailable),
        to_numeric: Some(synthetic_to_numeric),
        to_bool: Some(synthetic_to_bool),
        add_slow: Some(synthetic_add),
        compare_slow: Some(synthetic_compare),
        get_property: Some(synthetic_get_unavailable),
        set_property: Some(synthetic_set_unavailable),
        call: Some(synthetic_call_unavailable),
        new_array: Some(synthetic_new_unavailable),
        new_object: Some(synthetic_new_unavailable),
        shape_guard: Some(synthetic_shape_guard_unavailable),
        materialize_owner: Some(synthetic_materialize_owner_unavailable),
        get_element: Some(synthetic_get_unavailable),
        set_element: Some(synthetic_get_unavailable),
        to_propkey: Some(synthetic_map_out_in_unavailable),
        get_global: Some(synthetic_map_out_in_unavailable),
        call_constructor: Some(synthetic_map_call_unavailable),
        regexp: Some(synthetic_map_out_two_unavailable),
        binary_arith_slow: Some(synthetic_map_out_two_op_unavailable),
        unary_arith_slow: Some(synthetic_map_out_in_op_unavailable),
        fclosure: Some(synthetic_map_out_in_unavailable),
        get_var_ref: Some(synthetic_map_out_in_op_unavailable),
        put_var_ref: Some(synthetic_map_out_in_op_unavailable),
        close_loc: Some(synthetic_map_in_unavailable),
        set_name: Some(synthetic_map_out_in_unavailable),
        generic_op: Some(synthetic_map_out_two_op_unavailable),
        iterator_op: Some(synthetic_map_out_two_op_unavailable),
    };

/// Result observed after invoking a generated aggregate-return entry point.
#[derive(Clone, Copy, Debug)]
pub struct SyntheticOutcome {
    pub exit: rquickjs_core::qjs::JSJitExit,
    pub result: JSValueRepr,
}

/// Deep copy of every byte a generated entry can mutate through a synthetic
/// execution frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticFrameSnapshot {
    frame: Vec<u8>,
    arguments: Vec<JSValueRepr>,
    locals: Vec<JSValueRepr>,
    stack: Vec<JSValueRepr>,
    bytecode: Vec<u8>,
    poll_count: usize,
    poll_interrupt_at: usize,
    poll_capture_backtrace: bool,
    poll_backtrace: Vec<usize>,
}

/// Owns all buffers referenced by one synthetic `JSJitExecFrame`.
pub struct SyntheticFrame {
    frame: rquickjs_core::qjs::JSJitExecFrame,
    arguments: Vec<JSValueRepr>,
    locals: Vec<JSValueRepr>,
    stack: Vec<AlignedStackSlot>,
    bytecode: Vec<u8>,
    poll: Box<PollState>,
}

impl SyntheticFrame {
    pub fn new(arguments: &[JSValueRepr], local_count: usize, stack_size: usize) -> Self {
        let mut arguments = arguments.to_vec();
        let mut locals = vec![JSValueRepr::undefined(); local_count];
        // A real QuickJS value stack has allocated, suitably aligned backing
        // storage even when the live depth is zero.  Rust's empty `Vec`
        // sentinel is only aligned to `JSValueRepr`'s declared alignment and
        // is therefore not a valid synthetic `JSValue *` for the JIT's
        // stricter 16-byte slot-range contract.
        let stack_slots = stack_size
            .checked_add(rquickjs_core::qjs::JS_JIT_HELPER_SCRATCH_SLOTS as usize)
            .expect("synthetic stack capacity");
        let mut stack = vec![AlignedStackSlot(JSValueRepr::undefined()); stack_slots.max(1)];
        let bytecode = vec![0_u8];
        let poll = Box::new(PollState {
            count: AtomicUsize::new(0),
            interrupt_at: AtomicUsize::new(usize::MAX),
            capture_backtrace: AtomicBool::new(false),
            backtrace: Mutex::new(Vec::new()),
            argument_count: arguments.len(),
            local_count,
        });
        let stack_capacity = unsafe { stack.as_mut_ptr().add(stack_slots).cast() };
        let frame = rquickjs_core::qjs::JSJitExecFrame {
            struct_size: mem::size_of::<rquickjs_core::qjs::JSJitExecFrame>() as u32,
            flags: 0,
            rt: (&*poll as *const PollState).cast_mut().cast(),
            ctx: ptr::null_mut(),
            function_id: 0,
            generation: 0,
            arg_buf: arguments.as_mut_ptr().cast(),
            var_buf: locals.as_mut_ptr().cast(),
            stack_base: stack.as_mut_ptr().cast(),
            stack_top: stack.as_mut_ptr().cast(),
            bytecode_start: bytecode.as_ptr(),
            pc: bytecode.as_ptr(),
            result: JSValueRepr::undefined().into_raw(),
            entry: rquickjs_core::qjs::JSJitEntryHandle {
                struct_size: mem::size_of::<rquickjs_core::qjs::JSJitEntryHandle>() as u32,
                reserved: 0,
                entry: None,
                pin: ptr::null_mut(),
                stack_map_count: u32::MAX,
                helper_abi_version: rquickjs_core::qjs::QJSJIT_HELPER_ABI_VERSION,
            },
            runtime_api: &SYNTHETIC_RUNTIME_API,
            runtime_id: 0,
            frame_cookie: 0,
            stack_capacity,
        };
        Self {
            frame,
            arguments,
            locals,
            stack,
            bytecode,
            poll,
        }
    }

    pub fn set_bytecode(&mut self, bytecode: &[u8]) {
        self.bytecode.clear();
        self.bytecode.extend_from_slice(bytecode);
        self.frame.bytecode_start = self.bytecode.as_ptr();
        self.frame.pc = self.bytecode.as_ptr();
    }

    pub fn bytecode_start(&self) -> *const u8 {
        self.frame.bytecode_start
    }

    pub fn interrupt_on_poll(&mut self, poll: usize) {
        self.poll.interrupt_at.store(poll, Ordering::Release);
    }

    pub fn poll_count(&self) -> usize {
        self.poll.count.load(Ordering::Acquire)
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    pub fn capture_backtrace_on_next_poll(&mut self) {
        self.poll.capture_backtrace.store(true, Ordering::Release);
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    pub fn captured_backtrace(&self) -> Vec<usize> {
        self.poll
            .backtrace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn snapshot(&self) -> SyntheticFrameSnapshot {
        let frame = unsafe {
            std::slice::from_raw_parts(
                ptr::addr_of!(self.frame).cast::<u8>(),
                mem::size_of::<rquickjs_core::qjs::JSJitExecFrame>(),
            )
        }
        .to_vec();
        SyntheticFrameSnapshot {
            frame,
            arguments: self.arguments.clone(),
            locals: self.locals.clone(),
            stack: self.stack.iter().map(|slot| slot.0).collect(),
            bytecode: self.bytecode.clone(),
            poll_count: self.poll.count.load(Ordering::Acquire),
            poll_interrupt_at: self.poll.interrupt_at.load(Ordering::Acquire),
            poll_capture_backtrace: self.poll.capture_backtrace.load(Ordering::Acquire),
            poll_backtrace: self
                .poll
                .backtrace
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        }
    }

    pub fn set_local(&mut self, index: usize, value: JSValueRepr) {
        self.locals[index] = value;
    }

    pub fn set_stack(&mut self, values: &[JSValueRepr]) {
        assert!(values.len() <= self.stack.len());
        for (slot, value) in self.stack.iter_mut().zip(values) {
            slot.0 = *value;
        }
        self.frame.stack_top = unsafe { self.frame.stack_base.add(values.len()) };
    }

    pub fn stack_storage_address(&self) -> usize {
        self.stack.as_ptr() as usize
    }

    pub fn set_stack_bounds_raw(&mut self, base: usize, top: usize) {
        self.frame.stack_base = base as *mut rquickjs_core::qjs::JSValue;
        self.frame.stack_top = top as *mut rquickjs_core::qjs::JSValue;
    }

    /// Invokes an entry whose Cranelift signature has an sret pointer followed
    /// by the execution-frame pointer and no ordinary return values.
    #[cfg(feature = "compiler")]
    pub unsafe fn call(
        &mut self,
        executable: &crate::compiler::baseline::PublishedBaselineCode,
    ) -> SyntheticOutcome {
        type Entry = unsafe extern "C" fn(
            *mut rquickjs_core::qjs::JSJitExecFrame,
        ) -> rquickjs_core::qjs::JSJitExit;
        let entry: Entry = unsafe { mem::transmute(executable.as_ptr()) };
        let exit = unsafe { entry(ptr::addr_of_mut!(self.frame)) };
        #[cfg(rquickjs_memory_sanitizer)]
        unsafe {
            unpoison_native_outcome(ptr::addr_of!(exit), ptr::addr_of!(self.frame));
        }
        SyntheticOutcome {
            exit,
            result: JSValueRepr::from_raw(self.frame.result),
        }
    }
}

impl Drop for SyntheticFrame {
    fn drop(&mut self) {
        // Read the owned buffers so dead-code analysis cannot obscure that
        // their allocations intentionally pin every pointer in `frame`.
        let _ = (&self.arguments, &self.locals, &self.stack, &self.bytecode);
    }
}

pub fn verifier_metadata(
    osr_points: Vec<OsrPoint>,
    deopt_points: Vec<DeoptPoint>,
) -> VerifierMetadata {
    VerifierMetadata::new(osr_points, deopt_points)
}

#[derive(Clone)]
pub struct LifecycleRecorder {
    events: Arc<Mutex<Vec<&'static str>>>,
}

pub struct LifecycleRuntime {
    _guard: RuntimeJitGuard,
    runtime: Runtime,
}

impl LifecycleRuntime {
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }
}

struct RecordingBackend {
    events: Arc<Mutex<Vec<&'static str>>>,
}

unsafe impl JitBackend for RecordingBackend {
    fn runtime_detach(&mut self) {
        self.events.lock().unwrap().push("detach");
    }
}

impl Drop for RecordingBackend {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("backend_drop");
    }
}

pub fn record_lifecycle() -> LifecycleRecorder {
    LifecycleRecorder {
        events: Arc::new(Mutex::new(Vec::new())),
    }
}

impl LifecycleRecorder {
    pub fn runtime(&self) -> LifecycleRuntime {
        let runtime = Runtime::new().expect("test runtime");
        let drop_events = Arc::clone(&self.events);
        rquickjs_core::runtime::test_support::set_runtime_drop_probe(&runtime, move || {
            drop_events.lock().unwrap().push("runtime_drop");
        });
        let guard = RuntimeJitGuard::attach(
            &runtime,
            RecordingBackend {
                events: Arc::clone(&self.events),
            },
        )
        .expect("attach test backend");
        self.events.lock().unwrap().push("attach");
        LifecycleRuntime {
            _guard: guard,
            runtime,
        }
    }

    pub fn snapshot(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }

    pub fn take(&self) -> Vec<&'static str> {
        let mut events = self.events.lock().unwrap();
        core::mem::take(&mut *events)
    }
}

pub const fn fresh_bindgen_bindings() -> Option<&'static str> {
    rquickjs_core::runtime::test_support::fresh_bindgen_bindings()
}

struct DetachLabelBackend {
    events: Arc<Mutex<Vec<&'static str>>>,
    label: &'static str,
}

unsafe impl JitBackend for DetachLabelBackend {
    fn runtime_detach(&mut self) {
        self.events.lock().unwrap().push(self.label);
    }
}

pub fn duplicate_attachment_is_rejected() -> bool {
    let runtime = Runtime::new().expect("test runtime");
    let events = Arc::new(Mutex::new(Vec::new()));
    let first = RuntimeJitGuard::attach(
        &runtime,
        DetachLabelBackend {
            events: Arc::clone(&events),
            label: "first_detach",
        },
    )
    .expect("first attachment");
    let second = RuntimeJitGuard::attach(
        &runtime,
        DetachLabelBackend {
            events: Arc::clone(&events),
            label: "second_detach",
        },
    );
    let rejected = matches!(second, Err(JitBackendAttachError::AlreadyAttached));
    drop(first);
    rejected && *events.lock().unwrap() == ["first_detach"]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiMismatchFixture {
    SourceRevision,
    OpcodeFingerprint,
    ValueLayout,
    FeatureFlags,
    PointerWidth,
    Endianness,
    AbiInfoLayout,
    FunctionIdLayout,
    HotEventLayout,
    FunctionSnapshotLayout,
    EntryHandleLayout,
    ExecFrameLayout,
    ExitLayout,
    RuntimeApiLayout,
    HelperTable,
    ElementLayout,
    InlineApi,
    ArrayApi,
    IteratorApi,
    ObjectApi,
    BackendVTableLayout,
}

impl AbiMismatchFixture {
    pub const ALL: [Self; 21] = [
        Self::SourceRevision,
        Self::OpcodeFingerprint,
        Self::ValueLayout,
        Self::FeatureFlags,
        Self::PointerWidth,
        Self::Endianness,
        Self::AbiInfoLayout,
        Self::FunctionIdLayout,
        Self::HotEventLayout,
        Self::FunctionSnapshotLayout,
        Self::EntryHandleLayout,
        Self::ExecFrameLayout,
        Self::ExitLayout,
        Self::RuntimeApiLayout,
        Self::HelperTable,
        Self::ElementLayout,
        Self::InlineApi,
        Self::ArrayApi,
        Self::IteratorApi,
        Self::ObjectApi,
        Self::BackendVTableLayout,
    ];

    const fn mismatch(self) -> AbiMismatch {
        match self {
            Self::SourceRevision => AbiMismatch::SourceRevision,
            Self::OpcodeFingerprint => AbiMismatch::OpcodeFingerprint,
            Self::ValueLayout => AbiMismatch::ValueLayout,
            Self::FeatureFlags => AbiMismatch::FeatureFlags,
            Self::PointerWidth => AbiMismatch::PointerWidth,
            Self::Endianness => AbiMismatch::Endianness,
            Self::AbiInfoLayout => AbiMismatch::StructureLayout(AbiStructure::AbiInfo),
            Self::FunctionIdLayout => AbiMismatch::StructureLayout(AbiStructure::FunctionId),
            Self::HotEventLayout => AbiMismatch::StructureLayout(AbiStructure::HotEvent),
            Self::FunctionSnapshotLayout => {
                AbiMismatch::StructureLayout(AbiStructure::FunctionSnapshot)
            }
            Self::EntryHandleLayout => AbiMismatch::StructureLayout(AbiStructure::EntryHandle),
            Self::ExecFrameLayout => AbiMismatch::StructureLayout(AbiStructure::ExecFrame),
            Self::ExitLayout => AbiMismatch::StructureLayout(AbiStructure::Exit),
            Self::RuntimeApiLayout => AbiMismatch::StructureLayout(AbiStructure::RuntimeApi),
            Self::HelperTable => AbiMismatch::StructureLayout(AbiStructure::HelperTable),
            Self::ElementLayout => AbiMismatch::StructureLayout(AbiStructure::ElementLayout),
            Self::InlineApi => AbiMismatch::StructureLayout(AbiStructure::InlineApi),
            Self::ArrayApi => AbiMismatch::StructureLayout(AbiStructure::ArrayApi),
            Self::IteratorApi => AbiMismatch::StructureLayout(AbiStructure::IteratorApi),
            Self::ObjectApi => AbiMismatch::StructureLayout(AbiStructure::ObjectApi),
            Self::BackendVTableLayout => AbiMismatch::StructureLayout(AbiStructure::BackendVTable),
        }
    }
}

pub fn mismatch_is_rejected_before_attach(fixture: AbiMismatchFixture) -> bool {
    let runtime = Runtime::new().expect("test runtime");
    let mut info = AbiInfo::query_linked().expect("linked ABI");
    info.corrupt(fixture.mismatch());
    let diagnostics = Arc::new(AtomicUsize::new(0));
    let diagnostic_count = Arc::clone(&diagnostics);
    let config = JitConfig::builder()
        .diagnostic_callback(move |diagnostic| {
            if matches!(diagnostic.kind(), JitDiagnosticKind::AbiMismatch(_)) {
                diagnostic_count.fetch_add(1, Ordering::SeqCst);
            }
        })
        .build()
        .unwrap();
    let rejected = matches!(
        Jit::attach_with_info(&runtime, config, info),
        Err(JitError::Abi(_))
    ) && diagnostics.load(Ordering::SeqCst) == 1;
    if !rejected {
        return false;
    }

    // A valid attachment succeeding immediately afterward proves the rejected
    // fixture never stored its vtable in the runtime.
    Jit::attach(&runtime, JitConfig::default()).is_ok()
}
