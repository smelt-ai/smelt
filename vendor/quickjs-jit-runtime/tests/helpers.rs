#![cfg(feature = "test-support")]

use std::{
    ffi::c_void,
    mem, ptr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use rquickjs::{Context, Function, Runtime};
use rquickjs_core::{qjs, runtime::JitBackend};
use rquickjs_jit::{
    abi::JitExitExt,
    bytecode::{decode_raw, CompileSnapshot},
};

#[derive(Clone)]
enum Operation {
    DupFree,
    ValidatePcs {
        valid: Vec<usize>,
        invalid: Option<usize>,
        verified: Arc<AtomicBool>,
    },
    ResolveConst(u32),
    ToNumeric,
    ToBool,
    Add {
        stress: bool,
    },
    BinaryArith {
        opcode: u32,
        consumed: Arc<AtomicBool>,
    },
    UnaryArith {
        opcode: u32,
        consumed: Arc<AtomicBool>,
    },
    InvalidArith {
        binary: bool,
        opcode: u32,
        untouched: Arc<AtomicBool>,
    },
    Compare(u32),
    Get(u32),
    Set {
        atom: u32,
        consumed: Arc<AtomicBool>,
    },
    Call {
        stress: bool,
    },
    ShapeGuard {
        identity: u64,
        generation: u64,
        verified: Arc<AtomicBool>,
    },
    NewArray,
    NewObject,
    InspectScratch {
        retry: bool,
        verified: Arc<AtomicBool>,
    },
    PollForever,
    Invalid {
        kind: InvalidKind,
        untouched: Arc<AtomicBool>,
    },
}

#[derive(Clone, Copy, Debug)]
enum InvalidKind {
    RuntimeId,
    RuntimePointer(usize),
    ContextPointer(usize),
    RuntimeApi,
    Generation,
    Cookie,
    StackCapacity,
    StackTop,
    StackMap,
    Slot,
    ConstantIndex,
    HelperVersion,
    PcInsideInstruction,
    PcPastEnd,
}

#[derive(Clone)]
struct EntrySpec {
    id: u64,
    generation: u64,
    arg_count: u32,
    local_count: u32,
    stack_size: u32,
    operation: Operation,
}

struct HelperBackend {
    spec: EntrySpec,
    entries: Arc<AtomicUsize>,
}

unsafe impl JitBackend for HelperBackend {
    fn acquire_entry(&mut self, id: u64, generation: u64, pc: u32) -> qjs::JSJitEntryHandle {
        if id != self.spec.id || generation != self.spec.generation || pc != 0 {
            return qjs::JSJitEntryHandle {
                struct_size: mem::size_of::<qjs::JSJitEntryHandle>() as u32,
                reserved: 0,
                entry: None,
                pin: ptr::null_mut(),
                stack_map_count: 0,
                helper_abi_version: 0,
            };
        }
        self.entries.fetch_add(1, Ordering::SeqCst);
        qjs::JSJitEntryHandle {
            struct_size: mem::size_of::<qjs::JSJitEntryHandle>() as u32,
            reserved: 0,
            entry: Some(helper_entry),
            pin: Box::into_raw(Box::new(self.spec.clone())).cast::<c_void>(),
            stack_map_count: 2,
            helper_abi_version: qjs::QJSJIT_HELPER_ABI_VERSION,
        }
    }

    fn release_entry(&mut self, entry: qjs::JSJitEntryHandle) {
        if !entry.pin.is_null() {
            unsafe { drop(Box::from_raw(entry.pin.cast::<EntrySpec>())) };
        }
    }
}

unsafe fn set_stack_depth(frame: *mut qjs::JSJitExecFrame, depth: usize) {
    unsafe {
        for index in 0..depth {
            *(*frame).stack_base.add(index) = qjs::JS_UNDEFINED;
        }
        (*frame).stack_top = (*frame).stack_base.add(depth);
    }
}

unsafe fn finish_slot(frame: *mut qjs::JSJitExecFrame, slot: *mut qjs::JSValue) -> qjs::JSJitExit {
    unsafe {
        (*frame).result = *slot;
        *slot = qjs::JS_UNDEFINED;
    }
    qjs::JSJitExit::done()
}

unsafe extern "C" fn helper_entry(frame: *mut qjs::JSJitExecFrame) -> qjs::JSJitExit {
    unsafe {
        let spec = &*((*frame).entry.pin.cast::<EntrySpec>());
        let api = &*(*frame).runtime_api;
        let stack_slot = spec.arg_count + spec.local_count;
        match &spec.operation {
            Operation::ValidatePcs {
                valid,
                invalid,
                verified,
            } => {
                set_stack_depth(frame, 1);
                for offset in valid {
                    (*frame).pc = (*frame).bytecode_start.add(*offset);
                    if api.dup.expect("DUP helper")(frame, 0, stack_slot, 0) < 0 {
                        return qjs::JSJitExit::exception();
                    }
                    if api.free.expect("FREE helper")(frame, 1, stack_slot) < 0 {
                        return qjs::JSJitExit::exception();
                    }
                }
                if let Some(offset) = invalid {
                    let before = *(*frame).arg_buf;
                    (*frame).pc = (*frame).bytecode_start.wrapping_add(*offset);
                    let status = api.free.expect("FREE helper")(frame, 1, 0);
                    let after = *(*frame).arg_buf;
                    verified.store(
                        status < 0 && before.u.ptr == after.u.ptr && before.tag == after.tag,
                        Ordering::SeqCst,
                    );
                    return qjs::JSJitExit::exception();
                }
                verified.store(true, Ordering::SeqCst);
                (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 1);
                qjs::JSJitExit::done()
            }
            Operation::DupFree => {
                set_stack_depth(frame, 1);
                let status = api.dup.expect("DUP helper")(frame, 0, stack_slot, 0);
                if status < 0 {
                    return qjs::JSJitExit::exception();
                }
                let status = api.free.expect("FREE helper")(frame, 1, stack_slot);
                if status < 0 {
                    return qjs::JSJitExit::exception();
                }
                (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 1);
                qjs::JSJitExit::done()
            }
            Operation::ResolveConst(index) => {
                set_stack_depth(frame, 1);
                let status =
                    api.resolve_const.expect("RESOLVE_CONST helper")(frame, 0, stack_slot, *index);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).stack_base)
                }
            }
            Operation::ToNumeric => {
                let status = api.to_numeric.expect("TO_NUMERIC helper")(frame, 0, 0, 0);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).arg_buf)
                }
            }
            Operation::ToBool => {
                let status = api.to_bool.expect("TO_BOOL helper")(frame, 0, 0, 0);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).arg_buf)
                }
            }
            Operation::Add { stress } => {
                if *stress {
                    (*frame).flags |= qjs::JS_JIT_FRAME_STRESS_GC;
                }
                let status = api.add_slow.expect("ADD_SLOW helper")(frame, 0, 0, 0, 1);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).arg_buf)
                }
            }
            Operation::BinaryArith { opcode, consumed } => {
                (*frame).flags |= qjs::JS_JIT_FRAME_STRESS_GC;
                let status = api.binary_arith_slow.expect("BINARY_ARITH_SLOW helper")(
                    frame, 0, 0, 0, 1, *opcode,
                );
                // Both operands are consumed on success and on exception; the
                // output only holds a value on success.
                let right_cleared = qjs::JS_IsUndefined(*(*frame).arg_buf.add(1));
                let left_cleared = qjs::JS_IsUndefined(*(*frame).arg_buf);
                consumed.store(
                    right_cleared && (status >= 0 || left_cleared),
                    Ordering::SeqCst,
                );
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).arg_buf)
                }
            }
            Operation::UnaryArith { opcode, consumed } => {
                (*frame).flags |= qjs::JS_JIT_FRAME_STRESS_GC;
                set_stack_depth(frame, 1);
                let status = api.unary_arith_slow.expect("UNARY_ARITH_SLOW helper")(
                    frame, 0, stack_slot, 0, *opcode,
                );
                let input_cleared = qjs::JS_IsUndefined(*(*frame).arg_buf);
                let output_cleared = qjs::JS_IsUndefined(*(*frame).stack_base);
                consumed.store(
                    input_cleared && (status >= 0 || output_cleared),
                    Ordering::SeqCst,
                );
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).stack_base)
                }
            }
            Operation::InvalidArith {
                binary,
                opcode,
                untouched,
            } => {
                let before_left = *(*frame).arg_buf;
                let before_right = *(*frame).arg_buf.add(1);
                let status = if *binary {
                    api.binary_arith_slow.expect("BINARY_ARITH_SLOW helper")(
                        frame, 0, 0, 0, 1, *opcode,
                    )
                } else {
                    api.unary_arith_slow.expect("UNARY_ARITH_SLOW helper")(frame, 0, 0, 0, *opcode)
                };
                let after_left = *(*frame).arg_buf;
                let after_right = *(*frame).arg_buf.add(1);
                untouched.store(
                    status == qjs::JS_JIT_HELPER_EXCEPTION
                        && qjs::JS_HasException((*frame).ctx)
                        && before_left.tag == after_left.tag
                        && before_left.u.ptr == after_left.u.ptr
                        && before_right.tag == after_right.tag
                        && before_right.u.ptr == after_right.u.ptr,
                    Ordering::SeqCst,
                );
                qjs::JSJitExit::exception()
            }
            Operation::Compare(operation) => {
                let status =
                    api.compare_slow.expect("COMPARE_SLOW helper")(frame, 0, 0, 0, 1, *operation);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).arg_buf)
                }
            }
            Operation::Get(atom) => {
                set_stack_depth(frame, 1);
                let status =
                    api.get_property.expect("GET_PROPERTY helper")(frame, 0, stack_slot, 0, *atom);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).stack_base)
                }
            }
            Operation::Set { atom, consumed } => {
                let value = (*frame).arg_buf.add(1);
                let status = api.set_property.expect("SET_PROPERTY helper")(frame, 0, 0, *atom, 1);
                consumed.store(qjs::JS_IsUndefined(*value), Ordering::SeqCst);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 1);
                    qjs::JSJitExit::done()
                }
            }
            Operation::Call { stress } => {
                if *stress {
                    (*frame).flags |= qjs::JS_JIT_FRAME_STRESS_GC;
                }
                set_stack_depth(frame, 2);
                let status =
                    api.call.expect("CALL helper")(frame, 0, stack_slot + 1, 0, stack_slot, 1, 1);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).stack_base.add(1))
                }
            }
            Operation::ShapeGuard {
                identity,
                generation,
                verified,
            } => {
                let helper = api.shape_guard.expect("SHAPE_GUARD helper");
                let before = *(*frame).arg_buf;
                let invoke =
                    |frame: *mut qjs::JSJitExecFrame, slot, identity: u64, generation: u64| {
                        helper(
                            frame,
                            0,
                            slot,
                            identity as u32,
                            (identity >> 32) as u32,
                            generation as u32,
                            (generation >> 32) as u32,
                        )
                    };
                let exact = invoke(frame, 0, *identity, *generation);
                let wrong_identity = invoke(frame, 0, identity ^ 1, *generation);
                let wrong_generation = invoke(frame, 0, *identity, generation ^ 1);
                let after = *(*frame).arg_buf;

                set_stack_depth(frame, 1);
                *(*frame).stack_base = qjs::JS_MKVAL(qjs::JS_TAG_INT, 7);
                let primitive_before = *(*frame).stack_base;
                let primitive = invoke(frame, stack_slot, *identity, *generation);
                let primitive_after = *(*frame).stack_base;
                let no_exception = !qjs::JS_HasException((*frame).ctx);

                let unchanged = before.tag == after.tag
                    && before.u.ptr == after.u.ptr
                    && primitive_before.tag == primitive_after.tag
                    && primitive_before.u.int32 == primitive_after.u.int32;
                verified.store(
                    exact == qjs::JS_JIT_HELPER_OK
                        && wrong_identity == qjs::JS_JIT_HELPER_GUARD_MISS
                        && wrong_generation == qjs::JS_JIT_HELPER_GUARD_MISS
                        && primitive == qjs::JS_JIT_HELPER_GUARD_MISS
                        && no_exception
                        && unchanged,
                    Ordering::SeqCst,
                );
                (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, i32::from(unchanged));
                qjs::JSJitExit::done()
            }
            Operation::NewArray | Operation::NewObject => {
                set_stack_depth(frame, 1);
                let helper = if matches!(spec.operation, Operation::NewArray) {
                    api.new_array.expect("NEW_ARRAY helper")
                } else {
                    api.new_object.expect("NEW_OBJECT helper")
                };
                let status = helper(frame, 0, stack_slot);
                if status < 0 {
                    qjs::JSJitExit::exception()
                } else {
                    finish_slot(frame, (*frame).stack_base)
                }
            }
            Operation::InspectScratch { retry, verified } => {
                let logical_end = (*frame).stack_base.add(spec.stack_size as usize);
                let capacity_is_exact = (*frame).stack_capacity
                    == logical_end.add(qjs::JS_JIT_HELPER_SCRATCH_SLOTS as usize);
                let scratch_is_clear = capacity_is_exact
                    && (0..qjs::JS_JIT_HELPER_SCRATCH_SLOTS as usize)
                        .all(|index| qjs::JS_IsUndefined(*logical_end.add(index)));
                verified.store(capacity_is_exact && scratch_is_clear, Ordering::SeqCst);
                if !capacity_is_exact || !scratch_is_clear {
                    return qjs::JSJitExit::exception();
                }
                for index in 0..qjs::JS_JIT_HELPER_SCRATCH_SLOTS as usize {
                    *logical_end.add(index) = qjs::JS_MKVAL(qjs::JS_TAG_INT, index as i32 + 1);
                    *logical_end.add(index) = qjs::JS_UNDEFINED;
                }
                if *retry {
                    qjs::JSJitExit::retry_interpreter()
                } else {
                    (*frame).result = qjs::JS_MKVAL(qjs::JS_TAG_INT, 1);
                    qjs::JSJitExit::done()
                }
            }
            Operation::PollForever => loop {
                if api.interrupt_poll.expect("POLL helper")(frame) < 0 {
                    return qjs::JSJitExit::interrupt();
                }
            },
            Operation::Invalid { kind, untouched } => {
                let before = *(*frame).arg_buf;
                let saved_runtime = (*frame).rt;
                let saved_context = (*frame).ctx;
                let saved_runtime_id = (*frame).runtime_id;
                let saved_generation = (*frame).generation;
                let saved_cookie = (*frame).frame_cookie;
                let saved_runtime_api = (*frame).runtime_api;
                let saved_stack_capacity = (*frame).stack_capacity;
                let saved_stack_top = (*frame).stack_top;
                let saved_helper_version = (*frame).entry.helper_abi_version;
                let saved_pc = (*frame).pc;
                let status = match *kind {
                    InvalidKind::RuntimeId => {
                        (*frame).runtime_id ^= 1;
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::RuntimePointer(runtime) => {
                        (*frame).rt = runtime as *mut qjs::JSRuntime;
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::ContextPointer(context) => {
                        (*frame).ctx = context as *mut qjs::JSContext;
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::RuntimeApi => {
                        (*frame).runtime_api = ptr::null();
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::Generation => {
                        (*frame).generation ^= 1;
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::Cookie => {
                        (*frame).frame_cookie ^= 1;
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::StackCapacity => {
                        (*frame).stack_capacity = (*frame).stack_capacity.sub(1);
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::StackTop => {
                        (*frame).stack_top = (*frame).stack_capacity.wrapping_add(1);
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    // FREE intentionally does not require a stack map: it only
                    // consumes an already-materialized owned slot. Exercise
                    // stack-map range validation through DUP, which does use
                    // deopt metadata.
                    InvalidKind::StackMap => api.dup.expect("DUP helper")(frame, 2, stack_slot, 0),
                    InvalidKind::Slot => api.free.expect("FREE helper")(frame, 0, u32::MAX - 1),
                    InvalidKind::ConstantIndex => {
                        set_stack_depth(frame, 1);
                        api.resolve_const.expect("RESOLVE_CONST helper")(
                            frame,
                            0,
                            stack_slot,
                            u32::MAX,
                        )
                    }
                    InvalidKind::HelperVersion => {
                        (*frame).entry.helper_abi_version ^= 1;
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    // `return 'constant'` starts with a five-byte atom push,
                    // so offset 1 is inside an instruction. The boundary
                    // bitmap must reject it exactly like the linear scan.
                    InvalidKind::PcInsideInstruction => {
                        (*frame).pc = (*frame).bytecode_start.add(1);
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                    InvalidKind::PcPastEnd => {
                        (*frame).pc = (*frame).bytecode_start.wrapping_add(1 << 20);
                        api.free.expect("FREE helper")(frame, 0, 0)
                    }
                };
                (*frame).rt = saved_runtime;
                (*frame).ctx = saved_context;
                (*frame).runtime_id = saved_runtime_id;
                (*frame).generation = saved_generation;
                (*frame).frame_cookie = saved_cookie;
                (*frame).runtime_api = saved_runtime_api;
                (*frame).stack_capacity = saved_stack_capacity;
                (*frame).stack_top = saved_stack_top;
                (*frame).entry.helper_abi_version = saved_helper_version;
                (*frame).pc = saved_pc;
                let after = *(*frame).arg_buf;
                untouched.store(
                    status < 0 && before.u.ptr == after.u.ptr && before.tag == after.tag,
                    Ordering::SeqCst,
                );
                qjs::JSJitExit::exception()
            }
        }
    }
}

struct ShapeCaptureBackend(Arc<Mutex<Option<(u64, u64)>>>);

unsafe impl JitBackend for ShapeCaptureBackend {
    fn record_feedback(&mut self, event: &qjs::JSJitFeedbackEvent) {
        if event.kind == qjs::JSJitFeedbackKind_JS_JIT_FEEDBACK_PROPERTY
            && event.shape_identity != 0
            && event.shape_generation != 0
        {
            *self.0.lock().unwrap() = Some((event.shape_identity, event.shape_generation));
        }
    }
}

fn snapshot<'js>(ctx: &rquickjs::Ctx<'js>, function: &Function<'js>) -> CompileSnapshot {
    unsafe { CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_value().as_raw()) }
        .expect("supported bytecode function")
}

fn atom_operand(snapshot: &CompileSnapshot, opcode_name: &str) -> u32 {
    let instruction = decode_raw(snapshot.bytecode())
        .unwrap()
        .into_iter()
        .find(|instruction| instruction.opcode().name() == opcode_name)
        .unwrap_or_else(|| panic!("{opcode_name} in target bytecode"));
    u32::from_le_bytes(instruction.bytes()[1..5].try_into().unwrap())
}

fn install(
    runtime: &Runtime,
    snapshot: &CompileSnapshot,
    operation: Operation,
) -> (rquickjs_core::runtime::RuntimeJitGuard, Arc<AtomicUsize>) {
    let entries = Arc::new(AtomicUsize::new(0));
    let guard = runtime
        .attach_jit_backend(HelperBackend {
            spec: EntrySpec {
                id: snapshot.function_id(),
                generation: snapshot.generation(),
                arg_count: u32::from(snapshot.arg_count()),
                local_count: u32::from(snapshot.local_count()),
                stack_size: u32::from(snapshot.stack_size()),
                operation,
            },
            entries: Arc::clone(&entries),
        })
        .unwrap();
    (guard, entries)
}

#[test]
fn dup_and_free_are_balanced_without_changing_the_borrowed_input() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let (snapshot, rt) = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target(o) { return o }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        (snapshot(&ctx, &function), unsafe {
            qjs::JS_GetRuntime(ctx.as_raw().as_ptr())
        })
    });
    assert_eq!(unsafe { qjs::JS_JitResetHelperCounters(rt) }, 0);
    let (_guard, entries) = install(&runtime, &snapshot, Operation::DupFree);

    let result = context.with(|ctx| ctx.eval::<i32, _>("target({ marker: 1 })").unwrap());
    let mut counters = qjs::JSJitHelperCounters {
        struct_size: mem::size_of::<qjs::JSJitHelperCounters>() as u32,
        reserved: 0,
        dup_count: 0,
        free_count: 0,
    };
    assert_eq!(
        unsafe { qjs::JS_JitGetHelperCounters(rt, &mut counters) },
        0
    );
    assert_eq!(result, 1);
    assert_eq!(entries.load(Ordering::SeqCst), 1);
    assert_eq!((counters.dup_count, counters.free_count), (1, 1));
}

#[test]
fn resolve_const_duplicates_the_active_function_constant() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.target = function target() { return /helper-constant-identity/ }",
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let index = snapshot
        .constants()
        .iter()
        .find(|constant| constant.kind() == qjs::JSJitConstantKind_JS_JIT_CONSTANT_STRING)
        .expect("string constant")
        .index();
    let (_guard, entries) = install(&runtime, &snapshot, Operation::ResolveConst(index));

    let result = context.with(|ctx| ctx.eval::<String, _>("target()").unwrap());
    assert_eq!(result, "helper-constant-identity");
    assert_eq!(entries.load(Ordering::SeqCst), 1);
}

#[test]
fn add_slow_preserves_symbol_to_primitive_order_and_exception_order() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target(a, b) { return a + b }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let (_guard, entries) = install(&runtime, &snapshot, Operation::Add { stress: true });

    let result = context.with(|ctx| {
        ctx.eval::<String, _>(
            r#"
            (() => {
              const events = [];
              const left = { [Symbol.toPrimitive]() { events.push("left"); return "L" } };
              const right = { [Symbol.toPrimitive]() { events.push("right"); return "R" } };
              const value = target(left, right);
              return JSON.stringify([value, events]);
            })()
            "#,
        )
        .unwrap()
    });
    assert_eq!(result, r#"["LR",["left","right"]]"#);

    let thrown = context.with(|ctx| {
        ctx.eval::<String, _>(
            r#"
            (() => {
              const events = [];
              const left = { [Symbol.toPrimitive]() { events.push("left"); return 1 } };
              const right = { [Symbol.toPrimitive]() { events.push("right"); throw new Error("boom") } };
              try { target(left, right) } catch (error) { return JSON.stringify([error.message, events]) }
            })()
            "#,
        )
        .unwrap()
    });
    assert_eq!(thrown, r#"["boom",["left","right"]]"#);
    assert_eq!(entries.load(Ordering::SeqCst), 2);
}

#[test]
fn binary_arith_slow_matches_interpreter_semantics_and_consumes_both_operands() {
    let cases = [
        (
            "function target(a, b) { return a % b }",
            qjs::QJS_JIT_OP_MOD,
            "String(target(7, 3))",
            "1",
        ),
        (
            "function target(a, b) { return a % b }",
            qjs::QJS_JIT_OP_MOD,
            "String(Object.is(target(-4, 2), -0))",
            "true",
        ),
        (
            "function target(a, b) { return a % b }",
            qjs::QJS_JIT_OP_MOD,
            "String(target(5, 0))",
            "NaN",
        ),
        (
            "function target(a, b) { return a ** b }",
            qjs::QJS_JIT_OP_POW,
            "String(target(2, 10))",
            "1024",
        ),
        (
            "function target(a, b) { return a << b }",
            qjs::QJS_JIT_OP_SHL,
            "String(target(1, 33))",
            "2",
        ),
        (
            "function target(a, b) { return a >>> b }",
            qjs::QJS_JIT_OP_SHR,
            "String(target(-1, 0))",
            "4294967295",
        ),
        (
            "function target(a, b) { return a >> b }",
            qjs::QJS_JIT_OP_SAR,
            "String(target(-8, 1))",
            "-4",
        ),
        (
            "function target(a, b) { return a * b }",
            qjs::QJS_JIT_OP_MUL,
            "String(target('5', '2'))",
            "10",
        ),
        (
            "function target(a, b) { return a / b }",
            qjs::QJS_JIT_OP_DIV,
            "String(target(1, 4))",
            "0.25",
        ),
        (
            "function target(a, b) { return a & b }",
            qjs::QJS_JIT_OP_AND,
            "String(target(6, 3))",
            "2",
        ),
        (
            "function target(a, b) { return a | b }",
            qjs::QJS_JIT_OP_OR,
            "String(target(6n, 3n))",
            "7",
        ),
        (
            "function target(a, b) { return a ^ b }",
            qjs::QJS_JIT_OP_XOR,
            "String(target('6', 3))",
            "5",
        ),
        (
            "function target(a, b) { return a - b }",
            qjs::QJS_JIT_OP_SUB,
            "(() => { try { target({ valueOf() { throw new Error('x') } }, 1) } catch (error) { return error.message } })()",
            "x",
        ),
        (
            "function target(a, b) { return a % b }",
            qjs::QJS_JIT_OP_MOD,
            "(() => { try { target(1, { valueOf() { throw new Error('y') } }) } catch (error) { return error.message } })()",
            "y",
        ),
    ];
    for (definition, opcode, expression, expected) in cases {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let snapshot = context.with(|ctx| {
            ctx.eval::<(), _>(format!("globalThis.target = {definition}"))
                .unwrap();
            let function: Function<'_> = ctx.globals().get("target").unwrap();
            snapshot(&ctx, &function)
        });
        let consumed = Arc::new(AtomicBool::new(false));
        let (_guard, entries) = install(
            &runtime,
            &snapshot,
            Operation::BinaryArith {
                opcode: u32::from(opcode),
                consumed: Arc::clone(&consumed),
            },
        );
        let result = context.with(|ctx| ctx.eval::<String, _>(expression).unwrap());
        assert_eq!(result, expected, "{expression}");
        assert!(
            consumed.load(Ordering::SeqCst),
            "{expression} left operands"
        );
        assert_eq!(entries.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn unary_arith_slow_matches_interpreter_semantics_and_consumes_the_input() {
    let cases = [
        (
            "function target(v) { return -v }",
            qjs::QJS_JIT_OP_NEG,
            "String(target('3'))",
            "-3",
        ),
        (
            "function target(v) { return -v }",
            qjs::QJS_JIT_OP_NEG,
            "String(Object.is(target(0), -0))",
            "true",
        ),
        (
            "function target(v) { return ~v }",
            qjs::QJS_JIT_OP_NOT,
            "String(target('7'))",
            "-8",
        ),
        (
            "function target(v) { return +v }",
            qjs::QJS_JIT_OP_PLUS,
            "String(target('12'))",
            "12",
        ),
        (
            "function target(v) { return ++v }",
            qjs::QJS_JIT_OP_INC,
            "String(target('41'))",
            "42",
        ),
        (
            "function target(v) { return --v }",
            qjs::QJS_JIT_OP_DEC,
            "String(target(2147483648))",
            "2147483647",
        ),
        (
            "function target(v) { return -v }",
            qjs::QJS_JIT_OP_NEG,
            "(() => { try { target({ valueOf() { throw new Error('z') } }) } catch (error) { return error.message } })()",
            "z",
        ),
        (
            "function target(v) { return +v }",
            qjs::QJS_JIT_OP_PLUS,
            "(() => { try { target(1n) } catch (error) { return error.constructor.name } })()",
            "TypeError",
        ),
    ];
    for (definition, opcode, expression, expected) in cases {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let snapshot = context.with(|ctx| {
            ctx.eval::<(), _>(format!("globalThis.target = {definition}"))
                .unwrap();
            let function: Function<'_> = ctx.globals().get("target").unwrap();
            snapshot(&ctx, &function)
        });
        let consumed = Arc::new(AtomicBool::new(false));
        let (_guard, entries) = install(
            &runtime,
            &snapshot,
            Operation::UnaryArith {
                opcode: u32::from(opcode),
                consumed: Arc::clone(&consumed),
            },
        );
        let result = context.with(|ctx| ctx.eval::<String, _>(expression).unwrap());
        assert_eq!(result, expected, "{expression}");
        assert!(
            consumed.load(Ordering::SeqCst),
            "{expression} left its input"
        );
        assert_eq!(entries.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn arith_slow_helpers_reject_foreign_opcodes_without_touching_operands() {
    let cases = [
        (true, u32::from(qjs::QJS_JIT_OP_ADD)),
        (true, u32::from(qjs::QJS_JIT_OP_NEG)),
        (true, u32::MAX),
        (false, u32::from(qjs::QJS_JIT_OP_SUB)),
        (false, u32::from(qjs::QJS_JIT_OP_TYPEOF)),
        (false, u32::MAX),
    ];
    for (binary, opcode) in cases {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let snapshot = context.with(|ctx| {
            ctx.eval::<(), _>("globalThis.target = function target(a, b) { return a - b }")
                .unwrap();
            let function: Function<'_> = ctx.globals().get("target").unwrap();
            snapshot(&ctx, &function)
        });
        let untouched = Arc::new(AtomicBool::new(false));
        let (_guard, entries) = install(
            &runtime,
            &snapshot,
            Operation::InvalidArith {
                binary,
                opcode,
                untouched: Arc::clone(&untouched),
            },
        );
        let result = context.with(|ctx| ctx.eval::<(), _>("target({ marker: 1 }, { marker: 2 })"));
        assert!(result.is_err(), "binary={binary} opcode={opcode} returned");
        assert!(
            untouched.load(Ordering::SeqCst),
            "binary={binary} opcode={opcode} touched operands"
        );
        assert_eq!(entries.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn property_get_routes_getters_and_proxies_through_the_existing_catch_path() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target(o) { return o.x }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let atom = atom_operand(&snapshot, "get_field");
    let (_guard, entries) = install(&runtime, &snapshot, Operation::Get(atom));

    let getter = context.with(|ctx| {
        ctx.eval::<String, _>(
            "try { target({ get x() { throw new Error('getter') } }) } catch (e) { e.message }",
        )
        .unwrap()
    });
    let proxy = context.with(|ctx| {
        ctx.eval::<String, _>(
            "try { target(new Proxy({}, { get() { throw new Error('proxy') } })) } catch (e) { e.message }",
        )
        .unwrap()
    });
    assert_eq!((getter.as_str(), proxy.as_str()), ("getter", "proxy"));
    assert_eq!(entries.load(Ordering::SeqCst), 2);
}

#[test]
fn property_set_consumes_the_value_on_success_and_exception() {
    for (setter, expected) in [
        ("set x(v) { this.seen = v }", "7"),
        ("set x(v) { throw new Error('setter') }", "setter"),
    ] {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let snapshot = context.with(|ctx| {
            ctx.eval::<(), _>("globalThis.target = function target(o, v) { o.x = v }")
                .unwrap();
            let function: Function<'_> = ctx.globals().get("target").unwrap();
            snapshot(&ctx, &function)
        });
        let atom = atom_operand(&snapshot, "put_field");
        let consumed = Arc::new(AtomicBool::new(false));
        let (_guard, entries) = install(
            &runtime,
            &snapshot,
            Operation::Set {
                atom,
                consumed: Arc::clone(&consumed),
            },
        );
        let source = format!(
            "const o = {{ {setter} }}; try {{ target(o, 7); String(o.seen) }} catch (e) {{ e.message }}"
        );
        let result = context.with(|ctx| ctx.eval::<String, _>(source).unwrap());
        assert_eq!(result, expected);
        assert!(consumed.load(Ordering::SeqCst));
        assert_eq!(entries.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn shape_guard_matches_exact_rooted_shape_and_misses_without_mutating_values() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.rooted = { x: 42 }; globalThis.target = function target(o) { return o.x }",
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });

    let captured = Arc::new(Mutex::new(None));
    {
        let _guard = runtime
            .attach_jit_backend(ShapeCaptureBackend(Arc::clone(&captured)))
            .unwrap();
        context.with(|ctx| ctx.eval::<i32, _>("target(rooted)").unwrap());
    }
    let (identity, generation) = captured
        .lock()
        .unwrap()
        .expect("property feedback exposes the production shape token");

    runtime.run_gc();
    let verified = Arc::new(AtomicBool::new(false));
    let (_guard, entries) = install(
        &runtime,
        &snapshot,
        Operation::ShapeGuard {
            identity,
            generation,
            verified: Arc::clone(&verified),
        },
    );
    let result = context.with(|ctx| ctx.eval::<i32, _>("target(rooted)").unwrap());

    assert_eq!(result, 1);
    assert!(verified.load(Ordering::SeqCst));
    assert_eq!(entries.load(Ordering::SeqCst), 1);
}

#[test]
fn call_is_reentrant_and_keeps_borrowed_values_live_across_forced_cycle_gc() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target(fn, value) { return fn(value) }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let (_guard, entries) = install(&runtime, &snapshot, Operation::Call { stress: true });

    let result = context.with(|ctx| {
        ctx.eval::<i32, _>(
            r#"
            target((value) => {
              const cycle = {}; cycle.self = cycle;
              return value + 1;
            }, 41)
            "#,
        )
        .unwrap()
    });
    assert_eq!(result, 42);
    assert_eq!(entries.load(Ordering::SeqCst), 1);
}

#[test]
fn conversion_comparison_and_allocation_helpers_return_owned_values() {
    let cases = [
        (
            "function target(v) { return +v }",
            Operation::ToNumeric,
            "target('42')",
            "42",
        ),
        (
            "function target(v) { return !!v }",
            Operation::ToBool,
            "target({})",
            "true",
        ),
        (
            "function target(a,b) { return a < b }",
            Operation::Compare(qjs::JSJitCompareOp_JS_JIT_COMPARE_LT),
            "target('2','10')",
            "false",
        ),
        (
            "function target() { return [] }",
            Operation::NewArray,
            "Array.isArray(target())",
            "true",
        ),
        (
            "function target() { return {} }",
            Operation::NewObject,
            "Object.getPrototypeOf(target()) === Object.prototype",
            "true",
        ),
    ];
    for (definition, operation, expression, expected) in cases {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let snapshot = context.with(|ctx| {
            ctx.eval::<(), _>(format!("globalThis.target = {definition}"))
                .unwrap();
            let function: Function<'_> = ctx.globals().get("target").unwrap();
            snapshot(&ctx, &function)
        });
        let (_guard, entries) = install(&runtime, &snapshot, operation);
        let result = context.with(|ctx| {
            ctx.eval::<String, _>(format!("String({expression})"))
                .unwrap()
        });
        assert_eq!(result, expected);
        assert_eq!(entries.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn zero_logical_stack_has_two_explicit_clear_scratch_slots() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() {}")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    assert_eq!(
        snapshot.stack_size(),
        0,
        "fixture must cover zero-stack allocation"
    );
    let verified = Arc::new(AtomicBool::new(false));
    let (_guard, entries) = install(
        &runtime,
        &snapshot,
        Operation::InspectScratch {
            retry: false,
            verified: Arc::clone(&verified),
        },
    );

    let result = context.with(|ctx| ctx.eval::<i32, _>("target()").unwrap());
    assert_eq!(result, 1);
    assert!(verified.load(Ordering::SeqCst));
    assert_eq!(entries.load(Ordering::SeqCst), 1);
}

#[test]
fn scratch_tail_does_not_overlap_captured_local_var_refs() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.target = function target() { let a = 20, b = 22; return function () { return a + b } }",
        )
        .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let verified = Arc::new(AtomicBool::new(false));
    let (_guard, entries) = install(
        &runtime,
        &snapshot,
        Operation::InspectScratch {
            retry: true,
            verified: Arc::clone(&verified),
        },
    );

    let result = context.with(|ctx| ctx.eval::<i32, _>("target()()").unwrap());
    assert_eq!(result, 42);
    assert!(verified.load(Ordering::SeqCst));
    assert_eq!(entries.load(Ordering::SeqCst), 1);
}

#[test]
fn invalid_identity_map_index_and_slot_are_rejected_before_touching_values() {
    let wrong_runtime = Runtime::new().unwrap();
    let wrong_context = Context::full(&wrong_runtime).unwrap();
    let wrong_rt = wrong_context.with(|ctx| unsafe { qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });

    let kinds = [
        InvalidKind::RuntimeId,
        InvalidKind::RuntimePointer(wrong_rt as usize),
        InvalidKind::ContextPointer(0),
        InvalidKind::RuntimeApi,
        InvalidKind::Generation,
        InvalidKind::Cookie,
        InvalidKind::StackCapacity,
        InvalidKind::StackTop,
        InvalidKind::StackMap,
        InvalidKind::Slot,
        InvalidKind::ConstantIndex,
        InvalidKind::HelperVersion,
        InvalidKind::PcInsideInstruction,
        InvalidKind::PcPastEnd,
    ];
    for kind in kinds {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let other_context = Context::full(&runtime).unwrap();
        let kind = match kind {
            InvalidKind::ContextPointer(_) => InvalidKind::ContextPointer(
                other_context.with(|ctx| ctx.as_raw().as_ptr() as usize),
            ),
            kind => kind,
        };
        let snapshot = context.with(|ctx| {
            ctx.eval::<(), _>("globalThis.target = function target(value) { return 'constant' }")
                .unwrap();
            let function: Function<'_> = ctx.globals().get("target").unwrap();
            snapshot(&ctx, &function)
        });
        let untouched = Arc::new(AtomicBool::new(false));
        let (_guard, entries) = install(
            &runtime,
            &snapshot,
            Operation::Invalid {
                kind,
                untouched: Arc::clone(&untouched),
            },
        );
        let result = context.with(|ctx| ctx.eval::<(), _>("target({ marker: 1 })"));
        assert!(result.is_err(), "{kind:?} unexpectedly returned");
        assert!(untouched.load(Ordering::SeqCst), "{kind:?} touched input");
        assert_eq!(entries.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn poll_id_zero_remains_compatible_and_interrupts_native_infinite_work() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let snapshot = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.target = function target() { for (;;) {} }")
            .unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let interrupts = Arc::new(AtomicUsize::new(0));
    runtime.set_interrupt_handler({
        let interrupts = Arc::clone(&interrupts);
        Some(Box::new(move || {
            interrupts.fetch_add(1, Ordering::SeqCst);
            true
        }))
    });
    let (_guard, entries) = install(&runtime, &snapshot, Operation::PollForever);

    let result = context
        .with(|ctx| ctx.eval::<String, _>("try { target(); 'caught' } catch (_) { 'caught' }"));
    assert!(result.is_err(), "compiled interrupt became catchable");
    assert_eq!(interrupts.load(Ordering::SeqCst), 1);
    assert_eq!(entries.load(Ordering::SeqCst), 1);
}

#[test]
fn helper_pc_validation_preserves_boundaries_across_forward_and_backward_queries() {
    check_helper_pc_boundaries(
        "globalThis.target = function target(o) { let n = 123456; if (o) n += 234567; return n + o.marker }",
        false,
    );
}

#[test]
fn helper_pc_validation_checks_cached_prefix_and_uncached_tail_after_invalidation() {
    // Constant-width instructions place starts and operand bytes throughout
    // the packed cache's byte-offset domain and beyond its 256-byte prefix.
    let body = "if (o) n += 234567;".repeat(64);
    let source = format!(
        "globalThis.target = function target(o) {{ let n = 123456; {body} return n + o.marker }}"
    );
    check_helper_pc_boundaries(&source, true);
}

fn check_helper_pc_boundaries(source: &str, invalidate: bool) {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let original_snapshot = context.with(|ctx| {
        ctx.eval::<(), _>(source).unwrap();
        let function: Function<'_> = ctx.globals().get("target").unwrap();
        snapshot(&ctx, &function)
    });
    let mut boundaries = vec![0];
    for instruction in decode_raw(original_snapshot.bytecode()).unwrap() {
        boundaries.push(boundaries.last().unwrap() + instruction.bytes().len());
    }
    let length = original_snapshot.bytecode().len();
    assert_eq!(boundaries.last(), Some(&length));
    if invalidate {
        assert!(length > 256, "test must exercise the uncached tail");
    }
    assert!((0..length).any(|offset| !boundaries.contains(&offset)));
    let mut valid = Vec::new();
    if invalidate {
        let cached: Vec<_> = boundaries
            .iter()
            .copied()
            .filter(|offset| (1..256).contains(offset))
            .take(9)
            .collect();
        assert_eq!(cached.len(), 9);
        // Fill the eight FIFO entries with distinct nonzero starts. Repeated
        // hits must remain valid, and introducing a ninth start evicts the
        // oldest; visiting it again must succeed through the ordinary walk.
        valid.extend_from_slice(&cached[..8]);
        valid.extend_from_slice(&cached[..8]);
        valid.extend([cached[8], cached[0]]);
        // Repeated PC zero is valid even while unused cache bytes are zero.
        valid.extend([0, 0]);
    }
    valid.extend(boundaries.iter().copied());
    valid.extend(boundaries.iter().rev().copied());
    valid.extend(boundaries.iter().flat_map(|offset| [*offset, *offset]));
    // Exercise every operand byte after populating the cursor at many valid PCs.
    let invalid = (0..=length + 1)
        .filter(|offset| !boundaries.contains(offset))
        .map(Some)
        .chain([Some(usize::MAX), None]);
    for (index, invalid) in invalid.enumerate() {
        let current_snapshot = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("target").unwrap();
            if invalidate && index == 1 {
                // The first invocation populated the cache. Invalidate that
                // same bytecode object before checking its new generation.
                assert_eq!(
                    unsafe {
                        qjs::JS_JitInvalidateFunction(
                            ctx.as_raw().as_ptr(),
                            function.as_value().as_raw(),
                        )
                    },
                    qjs::JS_JIT_BACKEND_OK
                );
            }
            snapshot(&ctx, &function)
        });
        let verified = Arc::new(AtomicBool::new(false));
        let (_guard, entries) = install(
            &runtime,
            &current_snapshot,
            Operation::ValidatePcs {
                valid: valid.clone(),
                invalid,
                verified: verified.clone(),
            },
        );
        let result = context.with(|ctx| ctx.eval::<i32, _>("target({marker: 1})"));
        assert_eq!(result.is_err(), invalid.is_some(), "PC {invalid:?}");
        assert!(verified.load(Ordering::SeqCst), "PC {invalid:?}");
        assert_eq!(entries.load(Ordering::SeqCst), 1);
        if invalid.is_some() {
            context.with(|ctx| {
                let _ = ctx.catch();
            });
        }
    }
}
