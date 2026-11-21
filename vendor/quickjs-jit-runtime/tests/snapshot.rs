use rquickjs::{context::EvalOptions, Context, Runtime, Value};
use rquickjs_jit::bytecode::{
    opcode, CompileSnapshot, DecodeError, DeoptPoint, FunctionFlags, RuntimeConstants, SlotKind,
    SnapshotStatus, VerifierMetadata, VerifyLimits,
};
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(CompileSnapshot: Send, Sync);
assert_not_impl_any!(RuntimeConstants: Send, Sync);

#[test]
fn retained_snapshot_accounting_includes_capacity_and_verifier_metadata() {
    let mut bytes = Vec::with_capacity(4096);
    bytes.push(opcode::RETURN_UNDEF);
    let snapshot = CompileSnapshot::from_untrusted_bytecode(bytes, 0, 0, 0, 0);
    assert!(snapshot.retained_bytes() >= 4096);
    let before = snapshot.retained_bytes();
    let metadata = VerifierMetadata::new(
        vec![rquickjs_jit::bytecode::OsrPoint::new(
            0,
            vec![SlotKind::Float64; 64],
        )],
        vec![DeoptPoint::new(
            0,
            vec![SlotKind::Float64; 64],
            vec![SlotKind::Float64; 64],
        )],
    );
    let with_metadata = snapshot.with_metadata(metadata);
    assert!(with_metadata.retained_bytes() >= before + 192);
    assert_eq!(
        with_metadata.clone().retained_bytes(),
        with_metadata.retained_bytes()
    );
}

struct SnapshotFixture {
    runtime: Runtime,
    _context: Context,
    snapshot: CompileSnapshot,
    _runtime_constants: RuntimeConstants,
}

impl SnapshotFixture {
    fn compile(source: &str) -> Self {
        let runtime = Runtime::new().expect("snapshot runtime");
        let context = Context::full(&runtime).expect("snapshot context");
        let snapshot = context.with(|ctx| {
            let mut options = EvalOptions::default();
            options.strict = false;
            let function: Value<'_> = ctx.eval_with_options(source, options).unwrap();
            unsafe {
                CompileSnapshot::capture_with_runtime_constants(
                    &runtime,
                    ctx.as_raw().as_ptr(),
                    function.as_raw(),
                )
            }
            .expect("supported snapshot")
        });
        let (snapshot, runtime_constants) = snapshot;
        Self {
            runtime,
            _context: context,
            snapshot,
            _runtime_constants: runtime_constants,
        }
    }

    fn snapshot(&self) -> CompileSnapshot {
        self.snapshot.clone()
    }

    fn runtime_constant_count(&self) -> usize {
        self._runtime_constants.len()
    }
}

impl Drop for SnapshotFixture {
    fn drop(&mut self) {
        self.runtime.run_gc();
    }
}

fn snapshot_status(source: &str) -> SnapshotStatus {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let mut options = EvalOptions::default();
        options.strict = false;
        let function: Value<'_> = ctx.eval_with_options(source, options).unwrap();
        unsafe { CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_raw()) }
            .map_or_else(|status| status, |_| SnapshotStatus::Ok)
    })
}

#[test]
fn snapshot_survives_source_function_collection() {
    let fixture = SnapshotFixture::compile("function f(a) { return a + 1 } f");
    let snapshot = fixture.snapshot();
    drop(fixture);

    assert!(snapshot.bytecode().len() >= 4);
    assert_eq!(snapshot.arg_count(), 1);
    assert!(snapshot
        .decode()
        .unwrap()
        .iter()
        .any(|instruction| instruction.opcode().name() == "add"));
    assert_eq!(
        snapshot.source_revision(),
        rquickjs_jit::abi::SOURCE_REVISION
    );
    assert_eq!(
        snapshot.opcode_fingerprint(),
        rquickjs_jit::abi::OPCODE_FINGERPRINT
    );
}

#[test]
fn decoder_rejects_truncated_operand() {
    let bytes = [opcode::PUSH_I32, 1, 2];
    assert_eq!(
        rquickjs_jit::bytecode::decode_raw(&bytes).unwrap_err(),
        DecodeError::Truncated { pc: 0, size: 5 }
    );
}

#[test]
fn snapshot_copies_constant_descriptors_without_heap_addresses() {
    let fixture = SnapshotFixture::compile("function f() { return function nested() {} } f");
    let snapshot = fixture.snapshot();
    let descriptors = snapshot.constants();
    assert_eq!(fixture.runtime_constant_count(), descriptors.len());
    drop(fixture);

    assert!(!descriptors.is_empty());
    assert!(descriptors
        .iter()
        .enumerate()
        .all(|(index, descriptor)| descriptor.index() == index as u32));
    assert!(snapshot.decode().is_ok());
}

#[test]
fn snapshot_copies_numeric_constant_payloads_without_heap_pointers() {
    let fixture = SnapshotFixture::compile("function f() { return 1.5 } f");
    let descriptor = fixture
        .snapshot()
        .constants()
        .iter()
        .copied()
        .find(|descriptor| descriptor.tag() == rquickjs_core::qjs::JS_TAG_FLOAT64)
        .expect("Float64 descriptor");
    assert_eq!(descriptor.payload(), 1.5f64.to_bits());

    let heap_fixture = SnapshotFixture::compile("function f() { return 'owned' } f");
    assert!(heap_fixture
        .snapshot()
        .constants()
        .iter()
        .filter(|descriptor| descriptor.tag() < 0)
        .all(|descriptor| descriptor.payload() == 0));
}

#[test]
fn dropping_runtime_constants_releases_the_retained_source_function() {
    let runtime = Runtime::new().expect("snapshot runtime");
    let context = Context::full(&runtime).expect("snapshot context");
    let (snapshot, runtime_constants) = context.with(|ctx| {
        let function: Value<'_> = ctx
            .eval(
                r#"(() => {
                    const source = function source() {
                        return function constantPoolEntry() {};
                    };
                    globalThis.__jit_snapshot_weak = new WeakRef(source);
                    return source;
                })()"#,
            )
            .expect("source function");
        unsafe {
            CompileSnapshot::capture_with_runtime_constants(
                &runtime,
                ctx.as_raw().as_ptr(),
                function.as_raw(),
            )
        }
        .expect("supported snapshot")
    });

    runtime.run_gc();
    assert!(context.with(|ctx| {
        ctx.eval::<bool, _>("__jit_snapshot_weak.deref() !== undefined")
            .unwrap()
    }));

    drop(runtime_constants);
    runtime.run_gc();
    assert!(!context.with(|ctx| {
        ctx.eval::<bool, _>("__jit_snapshot_weak.deref() !== undefined")
            .unwrap()
    }));
    assert!(snapshot.decode().is_ok());
}

#[test]
fn unsupported_function_kinds_have_categorized_statuses() {
    assert_eq!(
        snapshot_status("(function* generated() { yield 1 })"),
        SnapshotStatus::Generator
    );
    assert_eq!(
        snapshot_status("(async function asynchronous() { return 1 })"),
        SnapshotStatus::Async
    );
    assert_eq!(
        snapshot_status("(function dynamic(s) { return eval(s) })"),
        SnapshotStatus::Eval
    );
    assert_eq!(
        snapshot_status("(function scoped(o) { with (o) { return value } })"),
        SnapshotStatus::With
    );
}

#[test]
fn float_constants_produce_float64_verifier_slots() {
    let fixture = SnapshotFixture::compile("function f() { return 1.5 } f");
    let snapshot = fixture.snapshot();
    let pc = snapshot
        .decode()
        .unwrap()
        .into_iter()
        .find(|instruction| instruction.opcode().name().starts_with("push_const"))
        .expect("floating point constant opcode")
        .pc();
    let metadata = VerifierMetadata::new(
        vec![],
        vec![DeoptPoint::new(pc, vec![], vec![SlotKind::Float64])],
    );
    assert!(snapshot
        .with_metadata(metadata)
        .verify(VerifyLimits::default())
        .is_ok());
}

#[test]
fn strict_and_non_strict_snapshots_preserve_this_mode() {
    let loose = SnapshotFixture::compile("function loose() { return this } loose").snapshot();
    let strict = SnapshotFixture::compile("function strict() { 'use strict'; return this } strict")
        .snapshot();

    assert!(!loose.flags().is_strict());
    assert!(strict.flags().is_strict());
    assert_eq!(loose.flags().bits(), 0);
    assert_eq!(strict.flags().bits(), 1);
    assert!(loose.verify(VerifyLimits::default()).is_ok());
    assert!(strict.verify(VerifyLimits::default()).is_ok());
}

#[test]
fn untrusted_push_this_requires_explicit_strictness_metadata() {
    let bytecode = vec![opcode::PUSH_THIS, opcode::RETURN];
    let loose = CompileSnapshot::from_untrusted_bytecode_with_flags(
        bytecode.clone(),
        0,
        0,
        0,
        0,
        FunctionFlags::non_strict(),
    );
    let strict = CompileSnapshot::from_untrusted_bytecode_with_flags(
        bytecode,
        0,
        0,
        0,
        0,
        FunctionFlags::strict(),
    );

    assert!(!loose.flags().is_strict());
    assert!(strict.flags().is_strict());
    assert!(loose.verify(VerifyLimits::default()).is_ok());
    assert!(strict.verify(VerifyLimits::default()).is_ok());
}
