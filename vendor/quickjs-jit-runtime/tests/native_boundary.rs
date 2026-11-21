use std::{
    collections::HashMap,
    ffi::c_void,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use rquickjs::{
    allocator::{Allocator, RustAllocator},
    Context, Function, Object, Runtime,
};
use rquickjs_core::{qjs, runtime::JitBackend};
use rquickjs_jit::{abi::JitExitExt, bytecode::CompileSnapshot};

#[derive(Clone)]
enum EntryKind {
    Done,
    Throw,
    Interrupt,
    Deopt {
        resume_pc: u32,
        counter: Arc<AtomicUsize>,
    },
    InvalidDeopt,
    Retry,
    ScratchOwner(ScratchExit),
    Malformed(MalformedExit),
    FailingInterrupt(Arc<AtomicBool>),
    FailingPrebuiltInterrupt(Arc<AtomicBool>),
    FailingMalformed(Arc<AtomicBool>),
}

impl EntryKind {
    fn entry(&self) -> unsafe extern "C" fn(*mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
        match self {
            Self::Done => native_done,
            Self::Throw => native_throw,
            Self::Interrupt => native_interrupt,
            Self::Deopt { .. } => native_deopt,
            Self::InvalidDeopt => native_invalid_deopt,
            Self::Retry => native_retry,
            Self::ScratchOwner(_) => native_scratch_owner,
            Self::Malformed(_) => native_malformed,
            Self::FailingInterrupt(_) => native_failing_interrupt,
            Self::FailingPrebuiltInterrupt(_) => native_failing_prebuilt_interrupt,
            Self::FailingMalformed(_) => native_failing_malformed,
        }
    }
}

#[derive(Clone, Copy)]
enum MalformedExit {
    DoneWithPendingAndResult,
    DoneExceptionWithoutPending,
    ExceptionWithPendingAndResult,
    InterruptWithResult,
    DeoptWithPendingAndResult,
    RetryWithPendingAndResult,
}

#[derive(Clone, Copy, Debug)]
enum ScratchExit {
    Done,
    Exception,
    Interrupt,
    Deopt,
    Retry,
}

#[derive(Clone)]
struct EntrySpec {
    generation: u64,
    pc: u32,
    kind: EntryKind,
}

struct EntryPin {
    kind: EntryKind,
}

struct NativeBackend {
    entries: HashMap<u64, EntrySpec>,
    releases: Arc<AtomicUsize>,
    retirements: Arc<Mutex<Vec<(u64, u64)>>>,
}

impl NativeBackend {
    fn one(snapshot: &CompileSnapshot, kind: EntryKind, releases: Arc<AtomicUsize>) -> Self {
        Self::one_at(snapshot, 0, kind, releases)
    }

    fn one_at(
        snapshot: &CompileSnapshot,
        pc: u32,
        kind: EntryKind,
        releases: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            entries: HashMap::from([(
                snapshot.function_id(),
                EntrySpec {
                    generation: snapshot.generation(),
                    pc,
                    kind,
                },
            )]),
            releases,
            retirements: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recording(retirements: Arc<Mutex<Vec<(u64, u64)>>>) -> Self {
        Self {
            entries: HashMap::new(),
            releases: Arc::new(AtomicUsize::new(0)),
            retirements,
        }
    }
}

unsafe impl JitBackend for NativeBackend {
    fn acquire_entry(&mut self, id: u64, generation: u64, pc: u32) -> qjs::JSJitEntryHandle {
        let Some(spec) = self
            .entries
            .get(&id)
            .filter(|spec| spec.generation == generation && spec.pc == pc)
        else {
            return qjs::JSJitEntryHandle {
                struct_size: std::mem::size_of::<qjs::JSJitEntryHandle>() as u32,
                reserved: 0,
                entry: None,
                pin: std::ptr::null_mut(),
                stack_map_count: 0,
                helper_abi_version: 0,
            };
        };

        let pin = Arc::new(EntryPin {
            kind: spec.kind.clone(),
        });
        qjs::JSJitEntryHandle {
            struct_size: std::mem::size_of::<qjs::JSJitEntryHandle>() as u32,
            reserved: 0,
            entry: Some(spec.kind.entry()),
            pin: Arc::into_raw(pin).cast_mut().cast::<c_void>(),
            stack_map_count: 0,
            helper_abi_version: qjs::QJSJIT_HELPER_ABI_VERSION,
        }
    }

    fn release_entry(&mut self, entry: qjs::JSJitEntryHandle) {
        if !entry.pin.is_null() {
            unsafe { drop(Arc::from_raw(entry.pin.cast::<EntryPin>())) };
            self.releases.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn function_retire(&mut self, id: u64, generation: u64) {
        self.retirements.lock().unwrap().push((id, generation));
    }
}

struct UnpinnedBackend;

struct CachingBackend {
    inner: NativeBackend,
    epoch: Arc<AtomicUsize>,
    acquisitions: Arc<AtomicUsize>,
}

unsafe impl JitBackend for CachingBackend {
    fn acquire_entry(&mut self, id: u64, generation: u64, pc: u32) -> qjs::JSJitEntryHandle {
        let entry = self.inner.acquire_entry(id, generation, pc);
        if !entry.pin.is_null() {
            self.acquisitions.fetch_add(1, Ordering::SeqCst);
        }
        entry
    }

    fn release_entry(&mut self, entry: qjs::JSJitEntryHandle) {
        self.inner.release_entry(entry);
    }

    fn entry_cache_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst) as u64
    }
}

#[test]
fn cached_native_entry_reuses_a_pin_until_epoch_change_and_releases_on_detach() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { return 1 }")
            .unwrap();
        snapshot(&ctx, &ctx.globals().get::<_, Function>("target").unwrap())
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let acquisitions = Arc::new(AtomicUsize::new(0));
    let epoch = Arc::new(AtomicUsize::new(1));
    let guard = runtime
        .attach_jit_backend(CachingBackend {
            inner: NativeBackend::one(&captured, EntryKind::Done, Arc::clone(&releases)),
            epoch: Arc::clone(&epoch),
            acquisitions: Arc::clone(&acquisitions),
        })
        .unwrap();
    let invoke = || {
        context.with(|ctx| {
            let function: Function = ctx.globals().get("target").unwrap();
            assert_eq!(function.call::<_, i32>(()).unwrap(), 42);
        })
    };
    invoke();
    invoke();
    assert_eq!(acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    guard.suspend().unwrap();
    context.with(|ctx| {
        let function: Function = ctx.globals().get("target").unwrap();
        assert_eq!(function.call::<_, i32>(()).unwrap(), 1);
    });
    guard.resume().unwrap();
    invoke();
    assert_eq!(acquisitions.load(Ordering::SeqCst), 1);
    epoch.store(2, Ordering::SeqCst);
    invoke();
    assert_eq!(acquisitions.load(Ordering::SeqCst), 2);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    epoch.store(0, Ordering::SeqCst);
    invoke();
    assert_eq!(acquisitions.load(Ordering::SeqCst), 3);
    assert_eq!(releases.load(Ordering::SeqCst), 3);
    epoch.store(3, Ordering::SeqCst);
    invoke();
    drop(guard);
    assert_eq!(acquisitions.load(Ordering::SeqCst), 4);
    assert_eq!(releases.load(Ordering::SeqCst), 4);
}

struct RecursiveCachePin {
    epoch: Arc<AtomicUsize>,
    invalidate: Arc<AtomicBool>,
    retire: Arc<AtomicBool>,
    active: AtomicBool,
}

struct ReentrantCacheRelease {
    ctx: *mut qjs::JSContext,
    key: (u64, u64),
    acquisitions: AtomicUsize,
    releases: AtomicUsize,
    reenter: AtomicBool,
}

unsafe extern "C" fn acquire_reentrant_release(
    opaque: *mut c_void,
    id: u64,
    generation: u64,
    pc: u32,
) -> qjs::JSJitEntryHandle {
    let state = unsafe { &*opaque.cast::<ReentrantCacheRelease>() };
    let mut entry: qjs::JSJitEntryHandle = unsafe { std::mem::zeroed() };
    entry.struct_size = std::mem::size_of_val(&entry) as u32;
    if state.key == (id, generation) && pc == 0 {
        state.acquisitions.fetch_add(1, Ordering::SeqCst);
        entry.entry = Some(native_done);
        entry.helper_abi_version = qjs::QJSJIT_HELPER_ABI_VERSION;
        entry.pin = Box::into_raw(Box::new(0u8)).cast();
    }
    entry
}

unsafe extern "C" fn release_with_reentry(opaque: *mut c_void, entry: qjs::JSJitEntryHandle) {
    let state = unsafe { &*opaque.cast::<ReentrantCacheRelease>() };
    unsafe { drop(Box::from_raw(entry.pin.cast::<u8>())) };
    state.releases.fetch_add(1, Ordering::SeqCst);
    if state.reenter.swap(false, Ordering::SeqCst) {
        unsafe {
            let global = qjs::JS_GetGlobalObject(state.ctx);
            let function = qjs::JS_GetPropertyStr(state.ctx, global, c"target".as_ptr());
            let value = qjs::JS_Call(state.ctx, function, qjs::JS_UNDEFINED, 0, ptr::null_mut());
            assert_eq!(qjs::JS_VALUE_GET_INT(value), 42);
            qjs::JS_FreeValue(state.ctx, function);
            qjs::JS_FreeValue(state.ctx, global);
        }
    }
}

unsafe extern "C" fn constant_cache_epoch(_opaque: *mut c_void) -> u64 {
    1
}

#[test]
fn reentrant_release_cannot_repopulate_entry_cache_during_detach() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { return 1 }")
            .unwrap();
        let function: Function = ctx.globals().get("target").unwrap();
        let captured = snapshot(&ctx, &function);
        let mut state = ReentrantCacheRelease {
            ctx: ctx.as_raw().as_ptr(),
            key: (captured.function_id(), captured.generation()),
            acquisitions: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            reenter: AtomicBool::new(false),
        };
        let mut vtable: qjs::JSJitBackendVTable = unsafe { std::mem::zeroed() };
        vtable.struct_size = std::mem::size_of_val(&vtable) as u32;
        vtable.acquire_entry = Some(acquire_reentrant_release);
        vtable.release_entry = Some(release_with_reentry);
        vtable.entry_cache_epoch = Some(constant_cache_epoch);
        let rt = unsafe { qjs::JS_GetRuntime(state.ctx) };
        assert_eq!(
            unsafe {
                qjs::JS_SetJitBackend(
                    rt,
                    &vtable,
                    (&mut state as *mut ReentrantCacheRelease).cast(),
                )
            },
            qjs::JS_JIT_BACKEND_OK
        );
        assert_eq!(function.call::<_, i32>(()).unwrap(), 42);
        state.reenter.store(true, Ordering::SeqCst);
        assert_eq!(
            unsafe { qjs::JS_SetJitBackend(rt, ptr::null(), ptr::null_mut()) },
            qjs::JS_JIT_BACKEND_OK
        );
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 2);
        assert_eq!(state.releases.load(Ordering::SeqCst), 2);
    });
}

struct RecursiveCacheBackend {
    key: (u64, u64),
    epoch: Arc<AtomicUsize>,
    invalidate: Arc<AtomicBool>,
    retire: Arc<AtomicBool>,
    exits: Arc<Mutex<Vec<(u64, u64)>>>,
    acquisitions: Arc<AtomicUsize>,
    releases: Arc<AtomicUsize>,
}

unsafe impl JitBackend for RecursiveCacheBackend {
    fn acquire_entry(&mut self, id: u64, generation: u64, pc: u32) -> qjs::JSJitEntryHandle {
        let mut entry: qjs::JSJitEntryHandle = unsafe { std::mem::zeroed() };
        entry.struct_size = std::mem::size_of_val(&entry) as u32;
        if (id, generation) == self.key && pc == 0 {
            self.acquisitions.fetch_add(1, Ordering::SeqCst);
            entry.entry = Some(recursive_cached_entry);
            entry.helper_abi_version = qjs::QJSJIT_HELPER_ABI_VERSION;
            entry.pin = Box::into_raw(Box::new(RecursiveCachePin {
                epoch: Arc::clone(&self.epoch),
                invalidate: Arc::clone(&self.invalidate),
                retire: Arc::clone(&self.retire),
                active: AtomicBool::new(false),
            }))
            .cast();
        }
        entry
    }

    fn release_entry(&mut self, entry: qjs::JSJitEntryHandle) {
        let pin = unsafe { Box::from_raw(entry.pin.cast::<RecursiveCachePin>()) };
        assert!(
            !pin.active.load(Ordering::SeqCst),
            "released an active frame's pin"
        );
        self.releases.fetch_add(1, Ordering::SeqCst);
    }

    fn entry_cache_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst) as u64
    }

    fn function_retire(&mut self, _id: u64, _generation: u64) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn native_exit(&mut self, id: u64, generation: u64, _pc: u32, _kind: u32) {
        self.exits.lock().unwrap().push((id, generation));
    }
}

unsafe extern "C" fn recursive_cached_entry(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let frame = &mut *frame;
        let pin = &*frame.entry.pin.cast::<RecursiveCachePin>();
        assert!(
            !pin.active.swap(true, Ordering::SeqCst),
            "shared an active pin"
        );
        let n = qjs::JS_VALUE_GET_INT(*frame.arg_buf);
        if n == 0 {
            qjs::JS_RunGC(frame.rt);
            if pin.retire.swap(false, Ordering::SeqCst) {
                let global = qjs::JS_GetGlobalObject(frame.ctx);
                let function = qjs::JS_GetPropertyStr(frame.ctx, global, c"target".as_ptr());
                assert_eq!(
                    qjs::JS_JitInvalidateFunction(frame.ctx, function),
                    qjs::JS_JIT_BACKEND_OK
                );
                qjs::JS_FreeValue(frame.ctx, function);
                qjs::JS_FreeValue(frame.ctx, global);
            }
            if pin.invalidate.swap(false, Ordering::SeqCst) {
                pin.epoch.fetch_add(1, Ordering::SeqCst);
            }
            frame.result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 0);
        } else {
            let global = qjs::JS_GetGlobalObject(frame.ctx);
            let function = qjs::JS_GetPropertyStr(frame.ctx, global, c"target".as_ptr());
            let mut argument = qjs::JS_MKVAL(qjs::JS_TAG_INT, n - 1);
            let value = qjs::JS_Call(frame.ctx, function, qjs::JS_UNDEFINED, 1, &mut argument);
            qjs::JS_FreeValue(frame.ctx, function);
            qjs::JS_FreeValue(frame.ctx, global);
            if qjs::JS_IsException(value) {
                pin.active.store(false, Ordering::SeqCst);
                return qjs::JSJitExit::exception();
            }
            assert_eq!(qjs::JS_VALUE_GET_TAG(value), qjs::JS_TAG_INT);
            frame.result = qjs::JS_MKVAL(qjs::JS_TAG_INT, qjs::JS_VALUE_GET_INT(value) + 1);
        }
        pin.active.store(false, Ordering::SeqCst);
    }
    qjs::JSJitExit::done()
}

#[test]
fn cached_entry_recursion_keeps_active_pins_alive_across_gc_and_invalidation() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target(n) { return -1 }")
            .unwrap();
        snapshot(&ctx, &ctx.globals().get::<_, Function>("target").unwrap())
    });
    let acquisitions = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let invalidate = Arc::new(AtomicBool::new(false));
    let retire = Arc::new(AtomicBool::new(false));
    let exits = Arc::new(Mutex::new(Vec::new()));
    let guard = runtime
        .attach_jit_backend(RecursiveCacheBackend {
            key: (captured.function_id(), captured.generation()),
            epoch: Arc::new(AtomicUsize::new(1)),
            invalidate: Arc::clone(&invalidate),
            retire: Arc::clone(&retire),
            exits: Arc::clone(&exits),
            acquisitions: Arc::clone(&acquisitions),
            releases: Arc::clone(&releases),
        })
        .unwrap();
    let invoke = |n| {
        context.with(|ctx| {
            let function: Function = ctx.globals().get("target").unwrap();
            assert_eq!(function.call::<_, i32>((n,)).unwrap(), n);
        })
    };
    invoke(0);
    invalidate.store(true, Ordering::SeqCst);
    invoke(3);
    assert_eq!(acquisitions.load(Ordering::SeqCst), 4);
    assert_eq!(releases.load(Ordering::SeqCst), 4);
    invoke(0);
    retire.store(true, Ordering::SeqCst);
    invoke(2);
    assert!(
        exits
            .lock()
            .unwrap()
            .iter()
            .all(|key| *key == (captured.function_id(), captured.generation())),
        "active frames must report the generation they entered: {:?}",
        exits.lock().unwrap()
    );
    context.with(|ctx| {
        let function: Function = ctx.globals().get("target").unwrap();
        assert_eq!(function.call::<_, i32>((0,)).unwrap(), -1);
    });
    assert_eq!(acquisitions.load(Ordering::SeqCst), 7);
    assert_eq!(releases.load(Ordering::SeqCst), 7);
    drop(guard);
    assert_eq!(releases.load(Ordering::SeqCst), 7);
}

#[test]
fn native_to_js_recursion_obeys_stack_limit_and_recovers_after_exception() {
    let runtime = Runtime::new().unwrap();
    // Leave room for instrumented C frames while bounding deep recursion.
    runtime.set_max_stack_size(512 * 1024);
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target(n) { return -1 }")
            .unwrap();
        snapshot(&ctx, &ctx.globals().get::<_, Function>("target").unwrap())
    });
    let acquisitions = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(RecursiveCacheBackend {
            key: (captured.function_id(), captured.generation()),
            epoch: Arc::new(AtomicUsize::new(1)),
            invalidate: Arc::new(AtomicBool::new(false)),
            retire: Arc::new(AtomicBool::new(false)),
            exits: Arc::new(Mutex::new(Vec::new())),
            acquisitions: Arc::clone(&acquisitions),
            releases: Arc::clone(&releases),
        })
        .unwrap();
    context.with(|ctx| {
        let function: Function = ctx.globals().get("target").unwrap();
        assert_eq!(
            function.call::<_, i32>((2,)).unwrap(),
            2,
            "shallow call before overflow"
        );
        assert!(function.call::<_, i32>((10_000,)).is_err());
        let exception = ctx.catch();
        let message: String = exception.as_object().unwrap().get("message").unwrap();
        assert!(message.contains("stack"), "{message}");
        assert_eq!(function.call::<_, i32>((2,)).unwrap(), 2);
    });
    assert!(acquisitions.load(Ordering::SeqCst) > 2);
    drop(guard);
    assert_eq!(
        acquisitions.load(Ordering::SeqCst),
        releases.load(Ordering::SeqCst)
    );
}

unsafe impl JitBackend for UnpinnedBackend {
    fn acquire_entry(&mut self, _id: u64, _generation: u64, _pc: u32) -> qjs::JSJitEntryHandle {
        qjs::JSJitEntryHandle {
            struct_size: std::mem::size_of::<qjs::JSJitEntryHandle>() as u32,
            reserved: 0,
            entry: Some(native_done),
            pin: std::ptr::null_mut(),
            stack_map_count: 0,
            helper_abi_version: qjs::QJSJIT_HELPER_ABI_VERSION,
        }
    }
}

#[derive(Clone, Copy)]
enum ReentrantPhase {
    Acquire,
    Entry,
}

#[derive(Clone, Copy)]
enum ReentrantAction {
    Detach,
    Replace,
}

struct ReentrantPin {
    phase: ReentrantPhase,
    action: ReentrantAction,
    status: Arc<AtomicI32>,
}

struct ReentrantBackend {
    rt: usize,
    phase: ReentrantPhase,
    action: ReentrantAction,
    status: Arc<AtomicI32>,
    releases: Arc<AtomicUsize>,
    detaches: Arc<AtomicUsize>,
}

unsafe impl JitBackend for ReentrantBackend {
    fn acquire_entry(&mut self, _id: u64, _generation: u64, _pc: u32) -> qjs::JSJitEntryHandle {
        if matches!(self.phase, ReentrantPhase::Acquire) {
            self.status.store(
                unsafe { attempt_backend_change(self.rt as *mut qjs::JSRuntime, self.action) },
                Ordering::SeqCst,
            );
        }

        let pin = Arc::new(ReentrantPin {
            phase: self.phase,
            action: self.action,
            status: Arc::clone(&self.status),
        });
        qjs::JSJitEntryHandle {
            struct_size: std::mem::size_of::<qjs::JSJitEntryHandle>() as u32,
            reserved: 0,
            entry: Some(native_reentrant),
            pin: Arc::into_raw(pin).cast_mut().cast::<c_void>(),
            stack_map_count: 0,
            helper_abi_version: qjs::QJSJIT_HELPER_ABI_VERSION,
        }
    }

    fn release_entry(&mut self, entry: qjs::JSJitEntryHandle) {
        unsafe { drop(Arc::from_raw(entry.pin.cast::<ReentrantPin>())) };
        self.releases.fetch_add(1, Ordering::SeqCst);
    }

    fn runtime_detach(&mut self) {
        self.detaches.fetch_add(1, Ordering::SeqCst);
    }
}

unsafe fn attempt_backend_change(
    rt: *mut qjs::JSRuntime,
    action: ReentrantAction,
) -> std::ffi::c_int {
    match action {
        ReentrantAction::Detach => unsafe {
            qjs::JS_SetJitBackend(rt, std::ptr::null(), std::ptr::null_mut())
        },
        ReentrantAction::Replace => {
            let replacement = qjs::JSJitBackendVTable {
                struct_size: std::mem::size_of::<qjs::JSJitBackendVTable>() as u32,
                record_hot: None,
                submit_snapshot: None,
                acquire_entry: None,
                release_entry: None,
                runtime_detach: None,
                function_retire: None,
                memory_used: None,
                native_enter: None,
                native_exit: None,
                record_feedback: None,
                entry_cache_epoch: None,
                entry_fast_grant: None,
            };
            unsafe { qjs::JS_SetJitBackend(rt, &replacement, std::ptr::null_mut()) }
        }
    }
}

unsafe extern "C" fn native_reentrant(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let pin = &*((*frame).entry.pin.cast::<ReentrantPin>());
        if matches!(pin.phase, ReentrantPhase::Entry) {
            pin.status.store(
                attempt_backend_change((*frame).rt, pin.action),
                Ordering::SeqCst,
            );
        }
        (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 42);
    }
    qjs::JSJitExit::done()
}

unsafe extern "C" fn native_done(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 42);
    }
    qjs::JSJitExit::done()
}

unsafe extern "C" fn native_throw(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    const MESSAGE: &[u8] = b"native throw";
    unsafe {
        let error = qjs::JS_NewStringLen(
            (*frame).ctx,
            MESSAGE.as_ptr().cast(),
            MESSAGE.len() as qjs::size_t,
        );
        qjs::JS_Throw((*frame).ctx, error);
    }
    qjs::JSJitExit::exception()
}

unsafe extern "C" fn native_interrupt(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    const MESSAGE: &[u8] = b"catchable primitive";
    unsafe {
        let value = qjs::JS_NewStringLen(
            (*frame).ctx,
            MESSAGE.as_ptr().cast(),
            MESSAGE.len() as qjs::size_t,
        );
        qjs::JS_Throw((*frame).ctx, value);
    }
    qjs::JSJitExit::interrupt()
}

unsafe extern "C" fn native_failing_interrupt(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let pin = &*((*frame).entry.pin.cast::<EntryPin>());
        let EntryKind::FailingInterrupt(fail_allocations) = &pin.kind else {
            return qjs::JSJitExit::retry_interpreter();
        };
        qjs::JS_Throw((*frame).ctx, qjs::JS_MKVAL(qjs::JS_TAG_INT, 17));
        fail_allocations.store(true, Ordering::SeqCst);
    }
    qjs::JSJitExit::interrupt()
}

unsafe extern "C" fn native_failing_malformed(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let pin = &*((*frame).entry.pin.cast::<EntryPin>());
        let EntryKind::FailingMalformed(fail_allocations) = &pin.kind else {
            return qjs::JSJitExit::retry_interpreter();
        };
        qjs::JS_Throw((*frame).ctx, qjs::JS_MKVAL(qjs::JS_TAG_INT, 23));
        fail_allocations.store(true, Ordering::SeqCst);
    }
    qjs::JSJitExit::done()
}

unsafe extern "C" fn native_failing_prebuilt_interrupt(
    frame: *mut qjs::JSJitExecFrame,
) -> qjs::JSJitExit {
    unsafe {
        let pin = &*((*frame).entry.pin.cast::<EntryPin>());
        let EntryKind::FailingPrebuiltInterrupt(fail_allocations) = &pin.kind else {
            return qjs::JSJitExit::retry_interpreter();
        };
        let error = take_global(frame, b"__jitPending\0");
        qjs::JS_SetUncatchableError((*frame).ctx, error);
        qjs::JS_Throw((*frame).ctx, error);
        fail_allocations.store(true, Ordering::SeqCst);
    }
    qjs::JSJitExit::interrupt()
}

unsafe extern "C" fn native_deopt(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let pin = &*((*frame).entry.pin.cast::<EntryPin>());
        let EntryKind::Deopt { resume_pc, counter } = &pin.kind else {
            return qjs::JSJitExit::retry_interpreter();
        };
        counter.fetch_add(1, Ordering::SeqCst);
        (*(*frame).stack_base) = qjs::JS_MKVAL(qjs::JS_TAG_INT, 7);
        (*frame).stack_top = (*frame).stack_base.add(1);
        qjs::JSJitExit::resume((*frame).bytecode_start.add(*resume_pc as usize))
    }
}

unsafe extern "C" fn native_invalid_deopt(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe { qjs::JSJitExit::resume((*frame).bytecode_start.add(2)) }
}

unsafe extern "C" fn native_retry(_frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    qjs::JSJitExit::retry_interpreter()
}

unsafe extern "C" fn native_scratch_owner(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let pin = &*((*frame).entry.pin.cast::<EntryPin>());
        let EntryKind::ScratchOwner(exit) = pin.kind else {
            return qjs::JSJitExit::retry_interpreter();
        };
        let scratch = (*frame)
            .stack_capacity
            .sub(qjs::JS_JIT_HELPER_SCRATCH_SLOTS as usize);
        *scratch = take_global(frame, b"__jitScratch\0");
        match exit {
            ScratchExit::Done => {
                (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 1);
                qjs::JSJitExit::done()
            }
            ScratchExit::Exception => {
                qjs::JS_Throw((*frame).ctx, qjs::JS_MKVAL(qjs::JS_TAG_INT, 17));
                qjs::JSJitExit::exception()
            }
            ScratchExit::Interrupt => qjs::JSJitExit::interrupt(),
            ScratchExit::Deopt => qjs::JSJitExit::resume((*frame).pc),
            ScratchExit::Retry => qjs::JSJitExit::retry_interpreter(),
        }
    }
}

unsafe fn take_global(frame: *mut qjs::JSJitExecFrame, name: &'static [u8]) -> qjs::JSValue {
    unsafe {
        let global = qjs::JS_GetGlobalObject((*frame).ctx);
        let value = qjs::JS_GetPropertyStr((*frame).ctx, global, name.as_ptr().cast());
        qjs::JS_SetPropertyStr(
            (*frame).ctx,
            global,
            name.as_ptr().cast(),
            qjs::JS_UNDEFINED,
        );
        qjs::JS_FreeValue((*frame).ctx, global);
        value
    }
}

unsafe extern "C" fn native_malformed(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let pin = &*((*frame).entry.pin.cast::<EntryPin>());
        let EntryKind::Malformed(kind) = pin.kind else {
            return qjs::JSJitExit::retry_interpreter();
        };

        if matches!(
            kind,
            MalformedExit::DoneWithPendingAndResult
                | MalformedExit::ExceptionWithPendingAndResult
                | MalformedExit::DeoptWithPendingAndResult
                | MalformedExit::RetryWithPendingAndResult
        ) {
            let pending = take_global(frame, b"__jitPending\0");
            qjs::JS_Throw((*frame).ctx, pending);
        }
        if matches!(
            kind,
            MalformedExit::DoneWithPendingAndResult
                | MalformedExit::ExceptionWithPendingAndResult
                | MalformedExit::InterruptWithResult
                | MalformedExit::DeoptWithPendingAndResult
                | MalformedExit::RetryWithPendingAndResult
        ) {
            (*frame).result = take_global(frame, b"__jitResult\0");
        }

        match kind {
            MalformedExit::DoneWithPendingAndResult => qjs::JSJitExit::done(),
            MalformedExit::DoneExceptionWithoutPending => {
                (*frame).result = qjs::JS_EXCEPTION;
                qjs::JSJitExit::done()
            }
            MalformedExit::ExceptionWithPendingAndResult => qjs::JSJitExit::exception(),
            MalformedExit::InterruptWithResult => qjs::JSJitExit::interrupt(),
            MalformedExit::DeoptWithPendingAndResult => qjs::JSJitExit::resume((*frame).pc),
            MalformedExit::RetryWithPendingAndResult => qjs::JSJitExit::retry_interpreter(),
        }
    }
}

fn snapshot<'js>(ctx: &rquickjs::Ctx<'js>, function: &Function<'js>) -> CompileSnapshot {
    unsafe { CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_value().as_raw()) }
        .expect("supported bytecode function")
}

struct FailingAllocator {
    fail: Arc<AtomicBool>,
}

unsafe impl Allocator for FailingAllocator {
    fn alloc(&mut self, size: usize) -> *mut u8 {
        if self.fail.load(Ordering::SeqCst) {
            ptr::null_mut()
        } else {
            RustAllocator.alloc(size)
        }
    }

    fn calloc(&mut self, count: usize, size: usize) -> *mut u8 {
        if self.fail.load(Ordering::SeqCst) {
            ptr::null_mut()
        } else {
            RustAllocator.calloc(count, size)
        }
    }

    unsafe fn dealloc(&mut self, pointer: *mut u8) {
        unsafe { RustAllocator.dealloc(pointer) };
    }

    unsafe fn realloc(&mut self, pointer: *mut u8, new_size: usize) -> *mut u8 {
        if self.fail.load(Ordering::SeqCst) {
            ptr::null_mut()
        } else {
            unsafe { RustAllocator.realloc(pointer, new_size) }
        }
    }

    unsafe fn usable_size(pointer: *mut u8) -> usize {
        unsafe { RustAllocator::usable_size(pointer) }
    }
}

fn failing_runtime(fail: &Arc<AtomicBool>) -> Runtime {
    Runtime::new_with_alloc(FailingAllocator {
        fail: Arc::clone(fail),
    })
    .expect("runtime initializes before allocation failure is enabled")
}

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn install_drop_value<'js>(ctx: &rquickjs::Ctx<'js>, name: &str, drops: Arc<AtomicUsize>) {
    let probe = DropProbe(drops);
    let function = Function::new(ctx.clone(), move || {
        let _ = &probe;
    })
    .unwrap();
    let object = Object::new(ctx.clone()).unwrap();
    object.set("probe", function).unwrap();
    ctx.globals().set(name, object).unwrap();
}

fn assert_malformed_exit_is_uncatchable_and_balanced(
    kind: MalformedExit,
    pending_value: bool,
    result_value: bool,
) {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() { return 1 };
            globalThis.wrapper = function wrapper() {
                try { target(); return "accepted"; }
                catch (_) { return "caught"; }
            };
            "#,
        )
        .unwrap();
        if pending_value {
            install_drop_value(&ctx, "__jitPending", Arc::clone(&drops));
        }
        if result_value {
            install_drop_value(&ctx, "__jitResult", Arc::clone(&drops));
        }
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::Malformed(kind),
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| ctx.eval::<String, _>("wrapper()"));

    assert!(
        result.is_err(),
        "malformed native exit reached interpreted JavaScript"
    );
    drop(result);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        usize::from(pending_value) + usize::from(result_value),
        "the malformed exit did not release every transferred value"
    );
    drop(guard);
}

fn assert_owned_scratch_exit_is_rejected_and_freed(exit: ScratchExit) {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() { return 1 };
            globalThis.wrapper = function wrapper() {
                try { target(); return "accepted"; }
                catch (_) { return "caught"; }
            };
            "#,
        )
        .unwrap();
        install_drop_value(&ctx, "__jitScratch", Arc::clone(&drops));
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::ScratchOwner(exit),
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| ctx.eval::<String, _>("wrapper()"));

    assert!(result.is_err(), "{exit:?} scratch owner reached JavaScript");
    drop(result);
    assert_eq!(drops.load(Ordering::SeqCst), 1, "{exit:?} leaked scratch");
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

fn assert_reentrant_backend_change_is_busy(phase: ReentrantPhase, action: ReentrantAction) {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let rt = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { return 1 }")
            .unwrap();
        unsafe { qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) }
    });
    let status = Arc::new(AtomicI32::new(i32::MAX));
    let releases = Arc::new(AtomicUsize::new(0));
    let detaches = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(ReentrantBackend {
            rt: rt as usize,
            phase,
            action,
            status: Arc::clone(&status),
            releases: Arc::clone(&releases),
            detaches: Arc::clone(&detaches),
        })
        .unwrap();

    let result = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        function.call::<_, i32>(()).unwrap()
    });

    assert_eq!(result, 42);
    assert_eq!(status.load(Ordering::SeqCst), qjs::JS_JIT_BACKEND_BUSY);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    assert_eq!(detaches.load(Ordering::SeqCst), 0);
    drop(guard);
    assert_eq!(detaches.load(Ordering::SeqCst), 1);
}

#[test]
fn backend_detach_during_acquire_is_busy_and_releases_the_captured_pin_once() {
    assert_reentrant_backend_change_is_busy(ReentrantPhase::Acquire, ReentrantAction::Detach);
}

#[test]
fn backend_replace_during_acquire_is_busy_and_releases_the_captured_pin_once() {
    assert_reentrant_backend_change_is_busy(ReentrantPhase::Acquire, ReentrantAction::Replace);
}

#[test]
fn backend_detach_during_entry_is_busy_and_releases_the_captured_pin_once() {
    assert_reentrant_backend_change_is_busy(ReentrantPhase::Entry, ReentrantAction::Detach);
}

#[test]
fn backend_replace_during_entry_is_busy_and_releases_the_captured_pin_once() {
    assert_reentrant_backend_change_is_busy(ReentrantPhase::Entry, ReentrantAction::Replace);
}

#[test]
fn native_return_enters_c_done_cleanup_and_releases_the_pin() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { return 1 }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::Done,
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        function.call::<_, i32>(()).unwrap()
    });

    assert_eq!(result, 42);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

#[test]
fn verified_loop_header_can_enter_native_code_from_an_osr_poll() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() {
                globalThis.started = true;
                while (globalThis.keepGoing) globalThis.keepGoing--;
                return 3;
            };
            globalThis.keepGoing = 65;
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let verified = captured
        .verify(Default::default())
        .expect("loop bytecode verifies before publishing an OSR entry");
    let osr_pc = verified
        .instructions()
        .iter()
        .map(|instruction| instruction.pc())
        .find(|pc| *pc != 0 && verified.control_flow_graph().is_loop_header(*pc))
        .expect("non-entry loop header");
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one_at(
            &captured,
            osr_pc,
            EntryKind::Done,
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        function.call::<_, i32>(()).unwrap()
    });

    assert_eq!(result, 42);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

#[test]
fn osr_retry_restarts_at_the_polled_pc_without_replaying_the_prefix() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.started = 0;
            globalThis.keepGoing = 65;
            globalThis.target = function target() {
                globalThis.started++;
                while (globalThis.keepGoing) globalThis.keepGoing--;
                return globalThis.started;
            };
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let verified = captured.verify(Default::default()).expect("verified loop");
    let osr_pc = verified
        .instructions()
        .iter()
        .map(|instruction| instruction.pc())
        .find(|pc| *pc != 0 && verified.control_flow_graph().is_loop_header(*pc))
        .expect("non-entry loop header");
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one_at(
            &captured,
            osr_pc,
            EntryKind::Retry,
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        function.call::<_, i32>(()).unwrap()
    });

    assert_eq!(result, 1, "RETRY replayed code before the OSR poll");
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

#[test]
fn native_entry_without_a_lifetime_pin_is_not_called() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { return 1 }")
            .unwrap();
    });
    let guard = runtime.attach_jit_backend(UnpinnedBackend).unwrap();

    let result = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        function.call::<_, i32>(()).unwrap()
    });

    assert_eq!(result, 1);
    drop(guard);
}

#[test]
fn native_exception_is_caught_by_the_existing_js_catch_path() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() { return "interpreted" };
            globalThis.wrapper = function wrapper() {
                try { target(); return "missed"; }
                catch (error) { return String(error); }
            };
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::Throw,
            Arc::clone(&releases),
        ))
        .unwrap();

    let caught = context.with(|ctx| ctx.eval::<String, _>("wrapper()").unwrap());

    assert_eq!(caught, "native throw");
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

#[test]
fn native_interrupt_remains_uncatchable() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() { return 1 };
            globalThis.wrapper = function wrapper() {
                try { target(); return false; }
                catch (_) { return true; }
            };
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::Interrupt,
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| ctx.eval::<bool, _>("wrapper()"));

    assert!(result.is_err(), "an uncatchable interrupt reached JS catch");
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

fn assert_pending_uncatchable_survives_get_and_rethrow(
    context: &Context,
    fail_allocations: &AtomicBool,
) {
    fail_allocations.store(false, Ordering::SeqCst);
    context.with(|ctx| unsafe {
        for _ in 0..2 {
            let pending = qjs::JS_GetException(ctx.as_raw().as_ptr());
            assert!(
                qjs::JS_IsUncatchableError(pending),
                "get transferred a catchable boundary exception"
            );
            qjs::JS_Throw(ctx.as_raw().as_ptr(), pending);
        }

        let pending = qjs::JS_GetException(ctx.as_raw().as_ptr());
        assert!(qjs::JS_IsUncatchableError(pending));
        qjs::JS_FreeValue(ctx.as_raw().as_ptr(), pending);

        let ordinary_catch: bool = ctx
            .eval("try { throw 29 } catch (_) { true }")
            .expect("an ordinary primitive remains catchable after recovery");
        assert!(ordinary_catch);
    });
}

#[test]
fn primitive_interrupt_is_uncatchable_when_error_allocation_fails() {
    let fail_allocations = Arc::new(AtomicBool::new(false));
    let runtime = failing_runtime(&fail_allocations);
    let first_context = Context::full(&runtime).unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() { return 1 };
            globalThis.wrapper = function wrapper() {
                try { target(); return false; }
                catch (_) { return true; }
            };
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    drop(first_context);

    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::FailingInterrupt(Arc::clone(&fail_allocations)),
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| ctx.eval::<bool, _>("wrapper()"));

    assert!(
        result.is_err(),
        "allocation failure made the primitive interrupt catchable"
    );
    assert_eq!(releases.load(Ordering::SeqCst), 1);

    fail_allocations.store(false, Ordering::SeqCst);
    context.with(|ctx| unsafe {
        qjs::JS_ResetUncatchableError(ctx.as_raw().as_ptr());
        let reset = qjs::JS_GetException(ctx.as_raw().as_ptr());
        assert!(
            !qjs::JS_IsUncatchableError(reset),
            "the public reset operation did not clear the transferred sentinel"
        );
        qjs::JS_FreeValue(ctx.as_raw().as_ptr(), reset);
    });

    let result_after_reset = context.with(|ctx| ctx.eval::<bool, _>("wrapper()"));
    assert!(
        result_after_reset.is_err(),
        "resetting one transfer permanently disarmed the runtime sentinel"
    );
    assert_eq!(releases.load(Ordering::SeqCst), 2);
    assert_pending_uncatchable_survives_get_and_rethrow(&context, &fail_allocations);
    drop(guard);
}

#[test]
fn backtrace_allocation_failure_cannot_disarm_an_uncatchable_interrupt() {
    let fail_allocations = Arc::new(AtomicBool::new(false));
    let runtime = failing_runtime(&fail_allocations);
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.__jitPending = new Error("prebuilt interrupt");
            delete globalThis.__jitPending.stack;
            globalThis.target = function target() { return 1 };
            globalThis.wrapper = function wrapper() {
                try { target(); return false; }
                catch (_) { return true; }
            };
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });

    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::FailingPrebuiltInterrupt(Arc::clone(&fail_allocations)),
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| ctx.eval::<bool, _>("wrapper()"));

    assert!(
        result.is_err(),
        "backtrace OOM replaced an uncatchable interrupt with a catchable value"
    );
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    assert_pending_uncatchable_survives_get_and_rethrow(&context, &fail_allocations);
    drop(guard);
}

#[test]
fn malformed_exit_is_uncatchable_when_error_allocation_fails() {
    let fail_allocations = Arc::new(AtomicBool::new(false));
    let runtime = failing_runtime(&fail_allocations);
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() { return 1 };
            globalThis.wrapper = function wrapper() {
                try { target(); return false; }
                catch (_) { return true; }
            };
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });

    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::FailingMalformed(Arc::clone(&fail_allocations)),
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| ctx.eval::<bool, _>("wrapper()"));

    assert!(
        result.is_err(),
        "allocation failure made malformed-exit normalization catchable"
    );
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    assert_pending_uncatchable_survives_get_and_rethrow(&context, &fail_allocations);
    drop(guard);
}

#[test]
fn malformed_done_with_pending_exception_frees_exception_and_transferred_result() {
    assert_malformed_exit_is_uncatchable_and_balanced(
        MalformedExit::DoneWithPendingAndResult,
        true,
        true,
    );
}

#[test]
fn malformed_done_rejects_exception_sentinel_without_a_pending_exception() {
    assert_malformed_exit_is_uncatchable_and_balanced(
        MalformedExit::DoneExceptionWithoutPending,
        false,
        false,
    );
}

#[test]
fn malformed_exception_exit_frees_its_forbidden_transferred_result() {
    assert_malformed_exit_is_uncatchable_and_balanced(
        MalformedExit::ExceptionWithPendingAndResult,
        true,
        true,
    );
}

#[test]
fn malformed_interrupt_exit_frees_its_forbidden_transferred_result() {
    assert_malformed_exit_is_uncatchable_and_balanced(
        MalformedExit::InterruptWithResult,
        false,
        true,
    );
}

#[test]
fn malformed_deopt_with_pending_exception_frees_exception_and_transferred_result() {
    assert_malformed_exit_is_uncatchable_and_balanced(
        MalformedExit::DeoptWithPendingAndResult,
        true,
        true,
    );
}

#[test]
fn malformed_retry_with_pending_exception_frees_exception_and_transferred_result() {
    assert_malformed_exit_is_uncatchable_and_balanced(
        MalformedExit::RetryWithPendingAndResult,
        true,
        true,
    );
}

#[test]
fn every_exit_requires_both_reserved_scratch_slots_to_be_clear() {
    for exit in [
        ScratchExit::Done,
        ScratchExit::Exception,
        ScratchExit::Interrupt,
        ScratchExit::Deopt,
        ScratchExit::Retry,
    ] {
        assert_owned_scratch_exit_is_rejected_and_freed(exit);
    }
}

#[test]
fn deopt_resumes_after_the_materialized_side_effect_exactly_once() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let captured = context.with(|ctx| {
        let interpreted_counter = Arc::clone(&counter);
        ctx.globals()
            .set(
                "tick",
                Function::new(ctx.clone(), move || {
                    interpreted_counter.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap(),
            )
            .unwrap();
        ctx.eval::<(), _>("globalThis.target = function target() { tick(); return 7 }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let return_pc = captured
        .decode()
        .unwrap()
        .into_iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .expect("explicit return opcode")
        .pc();
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::Deopt {
                resume_pc: return_pc,
                counter: Arc::clone(&counter),
            },
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        function.call::<_, i32>(()).unwrap()
    });

    assert_eq!(result, 7);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

#[test]
fn deopt_rejects_a_pc_inside_an_instruction_as_uncatchable_after_releasing_the_pin() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.target = function target() { return 1000 };
            globalThis.wrapper = function wrapper() {
                try { target(); return "accepted"; }
                catch (error) { return String(error); }
            };
            "#,
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        let captured = snapshot(&ctx, &function);
        assert!(captured
            .decode()
            .unwrap()
            .iter()
            .any(|instruction| instruction.pc() == 0 && instruction.size() > 2));
        captured
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::InvalidDeopt,
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| ctx.eval::<String, _>("wrapper()"));

    assert!(
        result.is_err(),
        "mid-instruction resume was caught by interpreted JavaScript"
    );
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    drop(guard);
}

#[test]
fn closure_objects_share_bytecode_identity_and_retire_only_at_finalization() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let retirements = Arc::new(Mutex::new(Vec::new()));
    let guard = runtime
        .attach_jit_backend(NativeBackend::recording(Arc::clone(&retirements)))
        .unwrap();

    let key = context.with(|ctx| {
        let make: Function<'_> = ctx
            .eval("(function make() { return function shared() { return 1 } })")
            .unwrap();
        let first: Function<'_> = make.call(()).unwrap();
        let second: Function<'_> = make.call(()).unwrap();
        let first_snapshot = snapshot(&ctx, &first);
        let second_snapshot = snapshot(&ctx, &second);
        assert_eq!(first_snapshot.function_id(), second_snapshot.function_id());
        assert_eq!(first_snapshot.generation(), second_snapshot.generation());
        (first_snapshot.function_id(), first_snapshot.generation())
    });

    runtime.run_gc();
    let matching: Vec<_> = retirements
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|event| *event == key)
        .collect();
    assert_eq!(matching, [key]);
    drop(guard);
}

#[test]
fn distinct_bytecode_allocations_with_identical_bodies_get_distinct_ids() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();

    let (first_id, second_id) = context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
            globalThis.first = function sameBody() { return 1 };
            globalThis.second = function sameBody() { return 1 };
            "#,
        )
        .unwrap();
        let first: Function<'_> = ctx.globals().get("first").unwrap();
        let second: Function<'_> = ctx.globals().get("second").unwrap();
        (
            snapshot(&ctx, &first).function_id(),
            snapshot(&ctx, &second).function_id(),
        )
    });

    assert_ne!(
        first_id, second_id,
        "identity was derived from byte content"
    );
}

#[test]
fn explicit_invalidation_keeps_identity_and_advances_generation() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let (function_id, old_generation) = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { return 1 }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        let captured = snapshot(&ctx, &function);
        (captured.function_id(), captured.generation())
    });
    let retirements = Arc::new(Mutex::new(Vec::new()));
    let guard = runtime
        .attach_jit_backend(NativeBackend::recording(Arc::clone(&retirements)))
        .unwrap();

    let new_generation = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        let status = unsafe {
            qjs::JS_JitInvalidateFunction(ctx.as_raw().as_ptr(), function.as_value().as_raw())
        };
        assert_eq!(status, qjs::JS_JIT_BACKEND_OK);
        let captured = snapshot(&ctx, &function);
        assert_eq!(captured.function_id(), function_id);
        captured.generation()
    });

    assert!(new_generation > old_generation);
    assert_eq!(
        retirements.lock().unwrap().as_slice(),
        [(function_id, old_generation)]
    );
    drop(guard);
}

#[test]
fn invalidation_prevents_acquiring_an_entry_from_the_retired_generation() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let captured = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { return 1 }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(NativeBackend::one(
            &captured,
            EntryKind::Done,
            Arc::clone(&releases),
        ))
        .unwrap();

    let result = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        let status = unsafe {
            qjs::JS_JitInvalidateFunction(ctx.as_raw().as_ptr(), function.as_value().as_raw())
        };
        assert_eq!(status, qjs::JS_JIT_BACKEND_OK);
        function.call::<_, i32>(()).unwrap()
    });

    assert_eq!(result, 1);
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    drop(guard);
}
