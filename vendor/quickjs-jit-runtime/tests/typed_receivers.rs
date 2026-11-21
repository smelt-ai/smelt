//! Typed-array receivers that are not entry arguments.
//!
//! `float64array-traversal` loads both views from an object property into
//! `const` locals before its loop. Tier 2 must guard those local receivers
//! once on the loop-entry edge (revalidating after polls) rather than running
//! the `length` accessor through the generic property bridge per iteration,
//! and every changed receiver must still match the same-version interpreter.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Runtime};
use rquickjs_jit::bytecode::VerifyLimits;
use rquickjs_jit::compiler::optimized::Tier2Compiler;
use rquickjs_jit::runtime::{
    ArrayAccess, ArrayHazards, ArrayMode, FeedbackTable, FunctionKey, ObservedType,
    PropertyAttributes, PrototypeDependencyToken, ShapeFeedbackTable, ShapeObservation, ShapeToken,
};
use rquickjs_jit::test_support::SnapshotFixture;
use rquickjs_jit::{Jit, JitConfig};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn JS_JitGetHelperCount(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        helper_id: u32,
        count: *mut u64,
    ) -> i32;
}

const TRAVERSAL: &str = "(function convertAndSum(iterations,seed,buffers){const ints=buffers.ints;const floats=buffers.floats;let sum=seed;for(let i=0;i<ints.length;i++){floats[i]=ints[i]*0.25+0.5;sum+=floats[i];}return sum})";

/// Lower with the feedback the benchmark produces: an Int32 source whose
/// length and elements are read, and a Float64 destination that is stored
/// and read back. `get_field` observations make the receiver loads guarded.
fn lower_traversal(source: &str, id: u64) -> Result<String, String> {
    let fixture = SnapshotFixture::compile(source);
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let return_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .unwrap()
        .pc();
    let names: Vec<_> = verified
        .instructions()
        .iter()
        .map(|instruction| (instruction.pc(), instruction.opcode().name()))
        .collect();
    let mut shapes = ShapeFeedbackTable::new(3);
    let mut offset = 0;
    for &(pc, name) in &names {
        if name == "get_field" {
            shapes.observe(
                key,
                pc,
                ShapeObservation::new(
                    ShapeToken::new(0x1000, 7),
                    PrototypeDependencyToken::new(0, 0),
                    offset,
                    PropertyAttributes::WRITABLE,
                    ObservedType::Object,
                ),
            );
            offset += 1;
        }
    }
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(
            key,
            &[
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Object,
            ],
        );
        // The first element load reads the Int32 source; later loads read
        // back the Float64 destination after its store.
        let mut loads = 0;
        for &(pc, name) in &names {
            match name {
                "get_length" => {
                    feedback.observe_array(
                        key,
                        pc,
                        ArrayAccess::Length,
                        ArrayMode::Int32,
                        ArrayHazards::NONE,
                    );
                }
                "get_array_el" => {
                    let mode = if loads == 0 {
                        ArrayMode::Int32
                    } else {
                        ArrayMode::Float64
                    };
                    loads += 1;
                    feedback.observe_array(key, pc, ArrayAccess::Load, mode, ArrayHazards::NONE);
                }
                "put_array_el" => {
                    feedback.observe_array(
                        key,
                        pc,
                        ArrayAccess::Store,
                        ArrayMode::Float64,
                        ArrayHazards::NONE,
                    );
                }
                "mul" | "add" => {
                    feedback.observe_binary(
                        key,
                        pc,
                        ObservedType::Float64,
                        ObservedType::Float64,
                        ObservedType::Float64,
                        Default::default(),
                    );
                }
                "lt" => {
                    feedback.observe_binary(
                        key,
                        pc,
                        ObservedType::Int32,
                        ObservedType::Int32,
                        ObservedType::Bool,
                        Default::default(),
                    );
                }
                _ => {}
            }
        }
        feedback.observe_return(key, return_pc, ObservedType::Float64);
    }
    Tier2Compiler::host(id)
        .lower_with_feedback_for_test(
            &verified,
            key,
            &feedback.snapshot(id).with_properties(shapes.snapshot(key)),
        )
        .map_err(|error| format!("{error:?}; {names:?}"))
}

/// Leaf metadata queries use a 24-byte `JSJitArrayMetadata` stack slot each.
fn metadata_queries(clif: &str) -> usize {
    clif.matches("explicit_slot 24").count()
}

/// Generic GET_PROPERTY calls pass `JS_ATOM_length` as an immediate operand.
fn generic_length_lookups(clif: &str) -> usize {
    let atom = format!("= {}", rquickjs_core::qjs::JS_ATOM_length);
    clif.lines()
        .filter(|line| line.contains("call_indirect") && line.contains(&atom))
        .count()
}

#[test]
fn local_typed_receivers_hoist_length_and_storage_out_of_the_loop() {
    let clif = lower_traversal(TRAVERSAL, 901).unwrap();
    assert_eq!(
        generic_length_lookups(&clif),
        0,
        "a guarded local Int32Array length still runs the accessor bridge: {clif}"
    );
    // One preheader query per receiver (ints with the intrinsic length flag,
    // floats storage-only) plus one revalidation each on the cold poll path.
    // No query remains on the hot loop path.
    assert_eq!(metadata_queries(&clif), 4, "{clif}");
    assert!(
        clif.contains("load.i32") && clif.contains("load.f64") && clif.contains("store"),
        "cached typed element accesses missing: {clif}"
    );
    // Only the floats store/load keep a bounds check; the ints load is
    // covered by the exact hoisted length.
    assert!(
        !clif.contains("icmp.i32 ult"),
        "the range-proven ints load retained a bounds check: {clif}"
    );
}

#[test]
fn rebound_or_aliased_local_receivers_keep_the_generic_length_path() {
    for source in [
        // The receiver local is rebound inside the loop.
        "(function f(iterations,seed,buffers){let ints=buffers.ints;const floats=buffers.floats;let sum=seed;for(let i=0;i<ints.length;i++){floats[i]=ints[i]*0.25+0.5;sum+=floats[i];ints=buffers.ints}return sum})",
        // Two locals hold the same receiver, so lowering provenance is
        // ambiguous.
        "(function f(iterations,seed,buffers){const ints=buffers.ints;const alias=ints;const floats=buffers.floats;let sum=seed;for(let i=0;i<ints.length;i++){floats[i]=alias[i]*0.25+0.5;sum+=floats[i];}return sum})",
    ] {
        // Declining to compile is also fail-closed.
        if let Ok(clif) = lower_traversal(source, 902) {
            assert!(
                generic_length_lookups(&clif) > 0 || metadata_queries(&clif) < 4,
                "an unproven local receiver was hoisted: {source}: {clif}"
            );
        }
    }
}

fn evaluate_without_jit(source: &str, expression: &str) -> String {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(source).unwrap();
        ctx.eval::<String, _>(expression).unwrap()
    })
}

const SOURCE: &str = r#"
    function convertAndSum(iterations, seed, buffers) {
      const ints = buffers.ints;
      const floats = buffers.floats;
      let sum = seed;
      for (let i = 0; i < ints.length; i++) {
        floats[i] = ints[i] * 0.25 + 0.5;
        sum += floats[i];
      }
      return sum;
    }
    function makeBuffers(n) {
      const ints = new Int32Array(n);
      for (let i = 0; i < n; i++) ints[i] = (i * 17) & 0xffff;
      return { ints: ints, floats: new Float64Array(n) };
    }
    globalThis.stable = makeBuffers(64);
"#;

fn prepared_traversal() -> (Runtime, Jit, Context) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(4)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(SOURCE)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics();
        context
            .with(|ctx| ctx.eval::<f64, _>("convertAndSum(0, 0, stable)"))
            .unwrap();
        let after = jit.metrics();
        if after.tier2_entries > before.tier2_entries
            && after.deopts == before.deopts
            && after.pending_worker_jobs == 0
        {
            return (runtime, jit, context);
        }
        jit.poll();
        assert!(
            Instant::now() < deadline,
            "local typed traversal never reached Tier2: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn helper_count(context: &Context, helper: u32) -> u64 {
    let rt =
        context.with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
    let mut count = 0;
    assert_eq!(unsafe { JS_JitGetHelperCount(rt, helper, &mut count) }, 0);
    count
}

#[test]
fn production_local_typed_traversal_avoids_the_length_accessor_bridge() {
    let (_runtime, jit, context) = prepared_traversal();
    let expected = evaluate_without_jit(SOURCE, "String(convertAndSum(0, 0, stable))");
    let get_property = rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY;
    let before_helpers = helper_count(&context, get_property);
    let before = jit.metrics();
    for _ in 0..16 {
        let actual = context
            .with(|ctx| ctx.eval::<String, _>("String(convertAndSum(0, 0, stable))"))
            .unwrap();
        assert_eq!(actual, expected);
    }
    let after = jit.metrics();
    assert_eq!(after.tier2_entries - before.tier2_entries, 16, "{after:?}");
    assert_eq!(after.deopts, before.deopts, "{after:?}");
    assert_eq!(after.native_entries, after.native_exits);
    // 16 calls x 64 iterations previously crossed the generic lookup bridge
    // for every `ints.length`. Only the two receiver loads may use a helper.
    let lookups = helper_count(&context, get_property) - before_helpers;
    assert!(
        lookups <= 32,
        "per-iteration generic property lookups remain: {lookups}"
    );
}

#[test]
fn changed_local_receivers_match_the_same_version_interpreter() {
    // Each probe changes one premise of the compiled loop: receiver mode,
    // extent, own/prototype `length`, attachment, aliasing, the property
    // load itself or the seed representation.
    const CASES: &str = r#"JSON.stringify([
        convertAndSum(0, 0, makeBuffers(0)),
        convertAndSum(0, 0, makeBuffers(3)),
        convertAndSum(0, 0, makeBuffers(1000)),
        convertAndSum(0, 0, {ints: new Int32Array([1, 2, 3]), floats: new Float64Array(2)}),
        convertAndSum(0, 0, {ints: new Float64Array([1.5, 2.5]), floats: new Float64Array(2)}),
        convertAndSum(0, 0, {ints: [1, 2, 3], floats: new Float64Array(3)}),
        convertAndSum(0, 0, {ints: new Int32Array([7, 9]), floats: new Int32Array(2)}),
        convertAndSum(0, 0, {ints: new Int32Array([7, 9]), floats: [0, 0]}),
        (() => { const a = new Int32Array([4, 5]); return convertAndSum(0, 0, {ints: a, floats: a}); })(),
        (() => { const buffer = new ArrayBuffer(64); const ints = new Int32Array(buffer); for (let i = 0; i < 16; i++) ints[i] = i * 3; return convertAndSum(0, 0, {ints: ints, floats: new Float64Array(buffer)}); })(),
        (() => { const b = makeBuffers(4); Object.defineProperty(b.ints, 'length', {value: 2}); return convertAndSum(0, 0, b); })(),
        (() => { const b = makeBuffers(4); b.floats.buffer.transfer(); return convertAndSum(0, 0, b); })(),
        (() => { const b = makeBuffers(4); b.ints.buffer.transfer(); return convertAndSum(0, 0, b); })(),
        (() => { let hits = 0; const b = makeBuffers(4); return [convertAndSum(0, 0, {get ints() { hits++; return b.ints; }, floats: b.floats}), hits]; })(),
        convertAndSum(0, 'seed:', makeBuffers(2)),
        convertAndSum(0, 0, stable),
        (() => { Object.defineProperty(Object.getPrototypeOf(Int32Array.prototype), 'length', {get() { return 2; }, configurable: true}); return convertAndSum(0, 0, makeBuffers(8)); })()
    ])"#;
    let expected = evaluate_without_jit(SOURCE, CASES);
    let (_runtime, jit, context) = prepared_traversal();
    let before = jit.metrics();
    let actual = context.with(|ctx| ctx.eval::<String, _>(CASES)).unwrap();
    let after = jit.metrics();
    assert_eq!(actual, expected);
    assert_eq!(after.native_entries, after.native_exits);
    assert!(
        after.tier2_entries > before.tier2_entries,
        "the probes never entered the prepared Tier2 body: {before:?} -> {after:?}"
    );
    assert!(
        after.deopts > before.deopts,
        "changed receivers must leave the guarded path: {before:?} -> {after:?}"
    );
}

#[test]
fn local_receiver_loop_revalidates_storage_after_observable_poll() {
    for detach_source in [false, true] {
        local_receiver_poll_case(detach_source);
    }
}

fn local_receiver_poll_case(detach_source: bool) {
    use rquickjs::qjs;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (runtime, jit, context) = prepared_traversal();
    const N: usize = 1_000_000;
    let (raw_ctx, raw_floats, raw_buffer) = context.with(|ctx| {
        // `fill` builds the large source in C; each term is 3 * 0.25 + 0.5.
        ctx.eval::<(), _>(format!(
            "globalThis.big = {{ints: new Int32Array({N}).fill(3), floats: new Float64Array({N})}}; globalThis.detachTarget = {}.buffer;",
            if detach_source { "big.ints" } else { "big.floats" }
        ))
        .unwrap();
        let big: rquickjs::Object = ctx.globals().get("big").unwrap();
        let floats: rquickjs::Object = big.get("floats").unwrap();
        let buffer: rquickjs::Object = ctx.globals().get("detachTarget").unwrap();
        (
            ctx.as_raw().as_ptr() as usize,
            unsafe { qjs::JS_VALUE_GET_PTR(floats.as_value().as_raw()) } as usize,
            unsafe { qjs::JS_VALUE_GET_PTR(buffer.as_value().as_raw()) } as usize,
        )
    });
    let changed = Arc::new(AtomicBool::new(false));
    let observed = changed.clone();
    // Detach once the loop has completed at least one store. The globally
    // rooted views stay live; detaching uses the C API without running script.
    runtime.set_interrupt_handler(Some(Box::new(move || unsafe {
        let ctx = raw_ctx as *mut qjs::JSContext;
        let floats = qjs::JS_MKPTR(qjs::JS_TAG_OBJECT, raw_floats as *mut core::ffi::c_void);
        let value = qjs::JS_GetPropertyUint32(ctx, floats, 1);
        let started = qjs::JS_VALUE_GET_TAG(value) == qjs::JS_TAG_FLOAT64
            && qjs::JS_VALUE_GET_FLOAT64(value) != 0.0;
        qjs::JS_FreeValue(ctx, value);
        if started && !observed.swap(true, Ordering::SeqCst) {
            let buffer = qjs::JS_MKPTR(qjs::JS_TAG_OBJECT, raw_buffer as *mut core::ffi::c_void);
            qjs::JS_DetachArrayBuffer(ctx, buffer);
        }
        false
    })));
    let before = jit.metrics();
    let result = context.with(|ctx| ctx.eval::<f64, _>("convertAndSum(0, 0, big)"));
    runtime.set_interrupt_handler(None);
    let result = result.unwrap();
    assert!(
        changed.load(Ordering::SeqCst),
        "no poll observed completed stores"
    );
    let full = N as f64 * 1.25;
    if detach_source {
        // A detached source reports length 0, ending the loop early.
        assert!(
            result.is_finite() && result < full,
            "loop used a stale source length after detach: {result}"
        );
    } else {
        // Every read of a detached destination is undefined.
        assert!(
            result.is_nan(),
            "loop used stale destination storage after detach: {result}"
        );
    }
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn packed_local_receivers_match_the_same_version_interpreter() {
    const PACKED: &str = r#"
        function sumSnapshot(o) {
          const values = o.values;
          let n = values.length, sum = 0;
          for (let i = 0; i < n; i++) sum = (sum + (values[i] | 0)) | 0;
          return sum;
        }
        function sumLive(o) {
          const values = o.values;
          let sum = 0;
          for (let i = 0; i < values.length; i++) sum = (sum + (values[i] | 0)) | 0;
          return sum;
        }
    "#;
    const CASES: &str = r#"JSON.stringify([
        sumSnapshot({values: []}), sumLive({values: []}),
        sumSnapshot({values: [7]}), sumLive({values: [7]}),
        sumSnapshot({values: [1,,3]}), sumLive({values: [1,,3]}),
        sumSnapshot({values: new Int32Array([4,5])}), sumLive({values: new Int32Array([4,5])}),
        sumSnapshot({values: [2147483647,1]}), sumLive({values: [2147483647,1]}),
        (() => { const a = [1,2,3]; a.length = 10; return [sumSnapshot({values: a}), sumLive({values: a})]; })(),
        sumSnapshot({values: [1.5, 'x', {}]}), sumLive({values: [1.5, 'x', {}]})
    ])"#;
    let expected = evaluate_without_jit(PACKED, CASES);
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(4)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(PACKED)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics();
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>(
                    "sumSnapshot({values: [1,2,3,4]}) + sumLive({values: [1,2,3,4]})"
                ))
                .unwrap(),
            20
        );
        let after = jit.metrics();
        if after.tier2_entries >= before.tier2_entries + 2 && after.pending_worker_jobs == 0 {
            break;
        }
        jit.poll();
        assert!(
            Instant::now() < deadline,
            "packed local traversals never reached Tier2: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = jit.metrics();
    let actual = context.with(|ctx| ctx.eval::<String, _>(CASES)).unwrap();
    let after = jit.metrics();
    assert_eq!(actual, expected);
    assert_eq!(after.native_entries, after.native_exits);
    assert!(after.tier2_entries > before.tier2_entries);
}

const NESTED: &str = r#"
    function storeNested(n, m, buffers) {
      const ints = buffers.ints;
      const floats = buffers.floats;
      let s = 0, t = 0;
      for (let i = 0; i < ints.length; i++) {
        floats[i] = ints[i] * 0.25 + 0.5;
        s += floats[i];
        for (let j = 0; j < m; j++) t = (t + 1) | 0;
      }
      return s;
    }
    function sumNested(f, m) {
      let s = 0, t = 0;
      for (let i = 0; i < f.length; i++) {
        s += f[i];
        for (let j = 0; j < m; j++) t = (t + 1) | 0;
      }
      return s;
    }
    function warmBuffers(n) {
      return {ints: new Int32Array(n).fill(4), floats: new Float64Array(n).fill(1.25)};
    }
"#;

/// A hoisted outer-loop tuple must be revalidated by every poll that can run
/// the interrupt handler, including the poll of a nested inner loop. With an
/// amortized countdown the inner loop consumes nearly every expiry, so a
/// revalidation only at the outer header would miss a detach performed there.
#[test]
fn outer_loop_hoists_are_revalidated_by_inner_loop_polls() {
    for (warm, call, target, detached_destination) in [
        (
            "storeNested(0, 2, warmBuffers(16))",
            "storeNested(0, 2000, big)",
            "big.floats.buffer",
            true,
        ),
        (
            "storeNested(0, 2, warmBuffers(16))",
            "storeNested(0, 2000, big)",
            "big.ints.buffer",
            false,
        ),
        (
            "sumNested(warmBuffers(16).floats, 2)",
            "sumNested(big.floats, 2000)",
            "big.floats.buffer",
            false,
        ),
    ] {
        nested_detach_case(warm, call, target, detached_destination);
    }
}

fn nested_detach_case(warm: &str, call: &str, target: &str, detached_destination: bool) {
    use rquickjs::qjs;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    const N: usize = 4000;
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(4)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(NESTED)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics();
        context.with(|ctx| ctx.eval::<f64, _>(warm)).unwrap();
        let after = jit.metrics();
        if after.tier2_entries > before.tier2_entries
            && after.deopts == before.deopts
            && after.pending_worker_jobs == 0
        {
            break;
        }
        jit.poll();
        assert!(
            Instant::now() < deadline,
            "{call}: nested traversal never reached Tier2: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let (raw_ctx, raw_buffer) = context.with(|ctx| {
        ctx.eval::<(), _>(format!(
            "globalThis.big = {{ints: new Int32Array({N}).fill(4), floats: new Float64Array({N}).fill(1.25)}}; globalThis.detachTarget = {target};"
        ))
        .unwrap();
        let buffer: rquickjs::Object = ctx.globals().get("detachTarget").unwrap();
        (
            ctx.as_raw().as_ptr() as usize,
            unsafe { qjs::JS_VALUE_GET_PTR(buffer.as_value().as_raw()) } as usize,
        )
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    // Detach on the third observable poll, well inside the outer loop. The
    // globally rooted buffer stays live; detaching runs no script.
    runtime.set_interrupt_handler(Some(Box::new(move || unsafe {
        if observed.fetch_add(1, Ordering::SeqCst) == 2 {
            let ctx = raw_ctx as *mut qjs::JSContext;
            let buffer = qjs::JS_MKPTR(qjs::JS_TAG_OBJECT, raw_buffer as *mut core::ffi::c_void);
            qjs::JS_DetachArrayBuffer(ctx, buffer);
        }
        false
    })));
    let before = jit.metrics();
    let result = context.with(|ctx| ctx.eval::<f64, _>(call));
    runtime.set_interrupt_handler(None);
    let result = result.unwrap();
    let after = jit.metrics();
    assert!(
        calls.load(Ordering::SeqCst) > 2,
        "{call}: the interrupt handler never detached"
    );
    assert!(
        after.tier2_entries > before.tier2_entries,
        "{call}: {after:?}"
    );
    assert_eq!(after.native_entries, after.native_exits);
    let full = N as f64 * 1.25;
    if detached_destination {
        // Every read of a detached destination is undefined.
        assert!(
            result.is_nan(),
            "{call}: stale destination storage used after detach: {result}"
        );
    } else {
        // A detached source reports length 0, ending the outer loop early.
        assert!(
            result.is_finite() && result < full,
            "{call}: stale source length used after detach: {result}"
        );
    }
    assert!(
        after.deopts > before.deopts,
        "{call}: native code never observed the detach: {after:?}"
    );
}

const CONDITIONAL_AND_WIDE: &str = r#"
    function maybeStore(n, o) {
      const src = o.src;
      const out = o.out;
      let s = 0;
      for (let i = 0; i < src.length; i++) {
        const v = src[i] * 0.5;
        if (n > 0) out[i] = v;
        s += v;
      }
      return s;
    }
    function fiveReceivers(n, o) {
      const a = o.a, b = o.b, c = o.c, d = o.d, e = o.e;
      let s = 0;
      for (let i = 0; i < a.length; i++) {
        s += a[i] + b[i] + c[i] + d[i];
        e[i] = s;
      }
      return s;
    }
    function views(n) {
      const f = (k) => new Float64Array(n).fill(k);
      return {src: new Int32Array(n).fill(3), out: new Float64Array(n),
              a: f(1), b: f(2), c: f(3), d: f(4), e: new Float64Array(n)};
    }
"#;

/// Storage hoists guard their receiver on the loop-entry edge only when every
/// iteration touches it, so an unused conditional destination stays native. More
/// than `MAX_LIVE_SOURCES` receivers keep exact per-access guards.
#[test]
fn conditional_and_wide_receiver_loops_match_the_interpreter() {
    const CASES: &str = r#"JSON.stringify([
        maybeStore(1, views(8)), maybeStore(0, {src: views(8).src, out: null}),
        maybeStore(1, {src: views(8).src, out: new Float64Array(3)}),
        maybeStore(1, {src: views(8).src, out: new Int32Array(8)}),
        fiveReceivers(0, views(8)),
        (() => { const v = views(8); v.c = new Float64Array(2); return fiveReceivers(0, v); })(),
        (() => { const v = views(8); v.e = v.a; return fiveReceivers(0, v); })()
    ])"#;
    let expected = evaluate_without_jit(CONDITIONAL_AND_WIDE, CASES);
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(4)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| ctx.eval::<(), _>(CONDITIONAL_AND_WIDE))
        .unwrap();
    context
        .with(|ctx| ctx.eval::<(), _>("globalThis.W = views(64)"))
        .unwrap();
    // Unoptimized (debug, coverage, sanitizer) builds compile Tier 2 so
    // slowly that the warm loop's own feedback keeps invalidating each
    // artifact's epoch. There, the Tier 2 assertions below are skipped but
    // the interpreter comparison still runs.
    let mut tier2_reached = true;
    for warm in ["maybeStore(1, W)", "fiveReceivers(0, W)"] {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            let before = jit.metrics();
            context.with(|ctx| ctx.eval::<f64, _>(warm)).unwrap();
            let after = jit.metrics();
            if after.tier2_entries > before.tier2_entries
                && after.deopts == before.deopts
                && after.pending_worker_jobs == 0
            {
                break;
            }
            jit.poll();
            if (cfg!(debug_assertions) || cfg!(rquickjs_sanitizer)) && Instant::now() >= deadline {
                tier2_reached = false;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{warm} never reached Tier2: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    // A destination that is not a typed array but is never stored to (the
    // store is conditional) must not deopt on every loop entry.
    context
        .with(|ctx| ctx.eval::<(), _>("globalThis.plainOut = {src: views(64).src, out: {}}"))
        .unwrap();
    let before = jit.metrics();
    for _ in 0..16 {
        context
            .with(|ctx| ctx.eval::<f64, _>("maybeStore(0, plainOut)"))
            .unwrap();
    }
    let after = jit.metrics();
    if tier2_reached {
        assert_eq!(after.tier2_entries - before.tier2_entries, 16, "{after:?}");
        assert_eq!(after.deopts, before.deopts, "{after:?}");
    }
    let actual = context.with(|ctx| ctx.eval::<String, _>(CASES)).unwrap();
    assert_eq!(actual, expected);
    let after = jit.metrics();
    assert_eq!(after.native_entries, after.native_exits);
}

/// Pre-existing at 82d3808: `const out = o.out` publishes a one-slot stack
/// top through the owned property bridge. When the loop-header numeric guard
/// then rejected the non-numeric `null` local, its exit resumed at the header
/// (stack depth 0) with that stale top, and the runtime rejected the exit as
/// "invalid native deoptimization metadata" (an uncatchable error).
#[test]
fn loop_header_guard_exit_republishes_an_empty_stack() {
    const SOURCE: &str = r#"
        function nullLocal(n, o) {
          const src = o.src;
          const out = o.out;
          let s = 0;
          for (let i = 0; i < src.length; i++) {
            const v = src[i] * 0.5;
            if (out) out[i] = v;
            s += v;
          }
          return s;
        }
        globalThis.nullOut = {src: new Int32Array(64).fill(3), out: null};
    "#;
    let expected = evaluate_without_jit(SOURCE, "String(nullLocal(0, nullOut))");
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(4)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(SOURCE)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    let mut entered = 0;
    while entered < 8 {
        let before = jit.metrics();
        let actual = context
            .with(|ctx| ctx.eval::<String, _>("String(nullLocal(0, nullOut))"))
            .unwrap();
        assert_eq!(actual, expected);
        let after = jit.metrics();
        entered += after.tier2_entries - before.tier2_entries;
        assert_eq!(after.native_entries, after.native_exits);
        jit.poll();
        assert!(
            Instant::now() < deadline,
            "nullLocal never reached Tier2: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// `const a = arg` keeps the entry argument's SSA identity, so the planner
/// can name `Argument` as the receiver while the operand provenance is
/// `Local`. A hoist-covered load must then keep its own bounds check (the
/// cached tuple, if any, is keyed by another slot). Out-of-range reads and
/// shrunk receivers must match the interpreter in whichever tier runs them.
#[test]
fn argument_aliased_local_receiver_loads_match_the_interpreter() {
    const SOURCE: &str = r#"
        function aliasSum(seed, values) {
          const a = values;
          let sum = seed;
          for (let i = 0; i < values.length; i++) sum = (sum + a[i]) | 0;
          return sum;
        }
        globalThis.A = new Int32Array(64).fill(3);
    "#;
    const CASES: &str = r#"JSON.stringify([
        aliasSum(0, A), aliasSum(0.5, A), aliasSum(1, new Int32Array(3).fill(5)),
        aliasSum(0, new Float64Array([1.5, 2.5])), aliasSum(1, [1, 2, 3]),
        (() => { const b = new Int32Array(8).fill(2); Object.defineProperty(b, 'length', {value: 12}); return aliasSum(0, b); })(),
        (() => { const b = new Int32Array(8).fill(1); b.buffer.transfer(); return aliasSum(0, b); })()
    ])"#;
    let expected = evaluate_without_jit(SOURCE, CASES);
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(4)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(SOURCE)).unwrap();
    // Today this shape is rejected by Tier 2 (fail-closed) and runs in a
    // lower tier; warm it without requiring a Tier 2 entry so the probes
    // stay exact whichever tier ends up executing them.
    for _ in 0..64 {
        context
            .with(|ctx| ctx.eval::<i32, _>("aliasSum(0, A)"))
            .unwrap();
        jit.poll();
    }
    let actual = context.with(|ctx| ctx.eval::<String, _>(CASES)).unwrap();
    assert_eq!(actual, expected);
    let after = jit.metrics();
    assert_eq!(after.native_entries, after.native_exits);
}
