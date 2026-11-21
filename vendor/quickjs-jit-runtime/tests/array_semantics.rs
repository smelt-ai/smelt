//! Native regression controls for Array and TypedArray observable semantics.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Runtime};
use rquickjs_jit::{Jit, JitConfig};
use std::time::{Duration, Instant};

fn length_case(initializer: &str, mutation: &str, expected: f64, expected_getters: i32) {
    let definition = format!(
        "globalThis.array={initializer};globalThis.getterHits=0;\
         function readLength(value){{return value.length;}}"
    );
    // Establish the same-version QuickJS result independently of native
    // compilation, including accessor effects and their original receiver.
    let interpreter = Runtime::new().unwrap();
    let interpreter_context = Context::full(&interpreter).unwrap();
    interpreter_context.with(|ctx| {
        ctx.eval::<(), _>(definition.as_str()).unwrap();
        ctx.eval::<(), _>(mutation).unwrap();
        assert_eq!(ctx.eval::<f64, _>("readLength(array)").unwrap(), expected);
        assert_eq!(ctx.eval::<i32, _>("getterHits").unwrap(), expected_getters);
    });

    let (_runtime, jit, context) = prepared_length_reader(&definition);
    context.with(|ctx| ctx.eval::<(), _>(mutation)).unwrap();
    let before = jit.metrics();
    let actual = context
        .with(|ctx| ctx.eval::<f64, _>("readLength(array)"))
        .unwrap();
    let after = jit.metrics();
    assert_eq!(
        after.tier2_entries - before.tier2_entries,
        1,
        "the changed receiver must enter the prepared Tier2 caller: {before:?} -> {after:?}"
    );
    assert_eq!(after.native_entries, after.native_exits);
    assert_eq!(
        actual, expected,
        "optimized length differs from QuickJS after: {mutation}"
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("getterHits"))
            .unwrap(),
        expected_getters
    );
}

fn prepared_length_reader(definition: &str) -> (Runtime, Jit, Context) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(definition)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics();
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<f64, _>("readLength(array)"))
                .unwrap(),
            2.0
        );
        let after = jit.metrics();
        if after.tier2_entries - before.tier2_entries == 1
            && after.native_entries - before.native_entries == 1
            && after.deopts == before.deopts
            && after.pending_worker_jobs == 0
        {
            break;
        }
        jit.poll();
        assert!(
            Instant::now() < deadline,
            "length accessor never reached Tier2: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    (runtime, jit, context)
}

#[test]
fn expanded_packed_array_length_is_logical_length_not_dense_count() {
    length_case("[1,2]", "array.length=10", 10.0, 0);
}

#[test]
fn packed_array_length_preserves_the_full_unsigned_range() {
    length_case("[1,2]", "array.length=4294967295", 4294967295.0, 0);
}

#[test]
fn typed_array_own_length_data_property_overrides_internal_count() {
    length_case(
        "new Int32Array([1,2])",
        "Object.defineProperty(array,'length',{value:17})",
        17.0,
        0,
    );
}

#[test]
fn typed_array_own_length_getter_runs_with_original_receiver() {
    length_case(
        "new Float64Array([1,2])",
        "Object.defineProperty(array,'length',{get(){if(this!==array)throw Error('receiver');getterHits++;return 23}})",
        23.0,
        1,
    );
}

#[test]
fn typed_array_prototype_length_getter_runs_with_original_receiver() {
    length_case(
        "new Int32Array([1,2])",
        "Object.defineProperty(Object.getPrototypeOf(Int32Array.prototype),'length',{configurable:true,get(){if(this!==array)throw Error('receiver');getterHits++;return 29}})",
        29.0,
        1,
    );
}

#[test]
fn typed_length_heap_result_survives_local_assignment_and_gc() {
    for body in [
        "return value.length",
        "let result=value.length;return result",
    ] {
        let (runtime, jit, context) = prepared_length_reader(&format!(
            "globalThis.array=new Int32Array(2);globalThis.getterHits=0;function readLength(value){{{body}}}"
        ));
        context.with(|ctx| ctx.eval::<(), _>(
            "Object.defineProperty(array,'length',{get(){if(this!==array)throw Error('receiver');getterHits++;return {marker:41,nested:{answer:42}}}})"
        )).unwrap();
        let before = jit.metrics();
        context
            .with(|ctx| ctx.eval::<(), _>("globalThis.saved=readLength(array)"))
            .unwrap();
        let after = jit.metrics();
        assert_eq!(
            after.tier2_entries - before.tier2_entries,
            1,
            "{body}: {before:?} -> {after:?}"
        );
        assert_eq!(after.native_entries, after.native_exits);
        runtime.run_gc();
        assert!(
            context
                .with(|ctx| ctx.eval::<bool, _>(
                    "getterHits===1 && saved.marker===41 && saved.nested.answer===42"
                ))
                .unwrap(),
            "getter result lost ownership or was replayed: {body}"
        );
        context
            .with(|ctx| ctx.eval::<(), _>("saved.nested.answer=43"))
            .unwrap();
        runtime.run_gc();
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("saved.nested.answer"))
                .unwrap(),
            43
        );
        context.with(|ctx| ctx.eval::<(), _>("saved=null")).unwrap();
        runtime.run_gc();
    }
}

#[test]
fn typed_length_string_result_survives_local_assignment_and_gc() {
    let (runtime, jit, context) = prepared_length_reader(
        "globalThis.array=new Float64Array(2);globalThis.getterHits=0;function readLength(value){let result=value.length;return result}"
    );
    context.with(|ctx| ctx.eval::<(), _>(
        "Object.defineProperty(Float64Array.prototype,'length',{get(){if(this!==array)throw Error('receiver');getterHits++;return 'owned-'+getterHits+'-payload'}})"
    )).unwrap();
    let before = jit.metrics();
    context
        .with(|ctx| ctx.eval::<(), _>("globalThis.saved=readLength(array)"))
        .unwrap();
    let after = jit.metrics();
    assert_eq!(after.tier2_entries - before.tier2_entries, 1);
    assert_eq!(after.native_entries, after.native_exits);
    runtime.run_gc();
    assert_eq!(
        context.with(|ctx| ctx.eval::<String, _>("saved")).unwrap(),
        "owned-1-payload"
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("getterHits"))
            .unwrap(),
        1
    );
    context.with(|ctx| ctx.eval::<(), _>("saved=null")).unwrap();
    runtime.run_gc();
}

#[test]
fn typed_length_throwing_getter_runs_once_and_preserves_exception_identity() {
    let (runtime, jit, context) = prepared_length_reader(
        "globalThis.array=new Int32Array(2);globalThis.getterHits=0;function readLength(value){let result=value.length;return result}"
    );
    context.with(|ctx| ctx.eval::<(), _>(
        "globalThis.token={marker:71};Object.defineProperty(array,'length',{configurable:true,get(){if(this!==array)throw Error('receiver');getterHits++;throw token}})"
    )).unwrap();
    let before = jit.metrics();
    assert!(context.with(|ctx| ctx.eval::<bool, _>(
        "globalThis.caught=null;globalThis.afterCall=0;try{readLength(array);afterCall++}catch(error){caught=error}caught===token && getterHits===1 && afterCall===0"
    )).unwrap());
    let after = jit.metrics();
    assert_eq!(after.tier2_entries - before.tier2_entries, 1);
    assert_eq!(after.native_entries, after.native_exits);
    runtime.run_gc();
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("caught.marker"))
            .unwrap(),
        71
    );
    context
        .with(|ctx| ctx.eval::<(), _>("delete array.length"))
        .unwrap();
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("readLength(array)"))
            .unwrap(),
        2
    );
}

#[test]
fn element_byte_offsets_widen_before_scaling_past_four_gibibytes() {
    use rquickjs_jit::compiler::optimized::Tier2Compiler;
    use rquickjs_jit::runtime::{
        ArrayAccess, ArrayHazards, ArrayMode, FeedbackTable, FunctionKey, ObservedType,
    };
    use rquickjs_jit::test_support::SnapshotFixture;

    // Exercising these indices against real packed/typed storage would require
    // more than 4 GiB of backing memory. Inspect actual emitted address
    // arithmetic instead: all operations here are element accesses, so every
    // multiply-immediate is a byte offset and must operate at pointer width.
    // Int32Array index 2^30, Float64Array index 2^29 and packed index 2^28
    // are valid Int32 indices whose byte offsets do not fit in 32 bits.
    for (source, arguments) in [
        (
            "(function(a,i){return a[i]})",
            vec![ObservedType::Object, ObservedType::Int32],
        ),
        (
            "(function(a,i,value){a[i]=value;return a[i]})",
            vec![
                ObservedType::Object,
                ObservedType::Int32,
                ObservedType::Int32,
            ],
        ),
    ] {
        let fixture = SnapshotFixture::compile(source);
        let verified = fixture.snapshot().verify(Default::default()).unwrap();
        let key = FunctionKey::new(
            verified.snapshot().function_id(),
            verified.snapshot().generation(),
        );
        for mode in [None, Some(ArrayMode::Int32), Some(ArrayMode::Float64)] {
            let mut feedback = FeedbackTable::new(32, 2);
            feedback.observe_call(key, &arguments);
            if let Some(mode) = mode {
                for instruction in verified.instructions() {
                    let access = match instruction.opcode().name() {
                        "get_array_el" => ArrayAccess::Load,
                        "put_array_el" => ArrayAccess::Store,
                        _ => continue,
                    };
                    feedback.observe_array(key, instruction.pc(), access, mode, ArrayHazards::NONE);
                }
            }
            let clif = Tier2Compiler::host(501)
                .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(501))
                .unwrap();
            let scales = clif
                .lines()
                .filter(|line| line.contains("imul_imm"))
                .collect::<Vec<_>>();
            assert!(
                !scales.is_empty(),
                "expected native element addressing: {clif}"
            );
            assert!(
                scales.iter().all(|line| !line.contains("imul_imm.i32")),
                "element byte offset wraps before pointer widening: {scales:?}"
            );
            let definitions = clif
                .lines()
                .filter_map(|line| line.trim().split_once(" = "))
                .collect::<std::collections::BTreeMap<_, _>>();
            for scale in scales {
                let operation = scale.trim().split_once(" = ").unwrap().1;
                let operand = operation
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .trim_end_matches(',');
                let definition = definitions
                    .get(operand)
                    .unwrap_or_else(|| panic!("missing scaled operand {operand}: {clif}"));
                assert!(definition.starts_with("uextend.i64 "), "byte offset must scale the widened index, not widen a wrapped product: {scale}; {operand} = {definition}");
            }
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

fn prepared_typed_store(source: &str, warmup: &str) -> (Runtime, Jit, Context) {
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
    context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics();
        context.with(|ctx| ctx.eval::<(), _>(warmup)).unwrap();
        jit.poll();
        let after = jit.metrics();
        if after.tier2_entries > before.tier2_entries && after.pending_worker_jobs == 0 {
            return (runtime, jit, context);
        }
        assert!(
            Instant::now() < deadline,
            "typed store never reached Tier2: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn typed_store_conversion_receiver_and_detach_match_interpreter() {
    for kind in ["Int32Array", "Float64Array"] {
        let source =
            format!("globalThis.a=new {kind}(4);function target(a,i,v){{a[i]=v;return a[i]}}");
        for probe in [
            "JSON.stringify([target([5],0,7),target(a,-1,9)===undefined,target(a,4,9)===undefined])",
            "JSON.stringify([target(a,0,4294967297.75),target(a,1,-0),1/a[1]])",
            "(()=>{let hits=0;let key={toString(){hits++;a[1]=23;return '0'}};return JSON.stringify([target(a,key,17),a[1],hits])})()",
            "(()=>{let hits=0;let value={valueOf(){hits++;a.buffer.transfer();return 17}};return JSON.stringify([target(a,0,value)===undefined,a.length,hits])})()",
            "(()=>{let hits=0,token={};try{target(a,0,{valueOf(){hits++;throw token}})}catch(e){return JSON.stringify([e===token,hits,a[0]])}return 'missing throw'})()",
            "(()=>{let hits=0;let p=new Proxy([0],{set(o,k,v){hits++;o[k]=v;return true}});return JSON.stringify([target(p,0,19),hits])})()",
            "(()=>{a.buffer.transfer();return JSON.stringify([target(a,0,17)===undefined,a.length])})()",
            "(()=>{Object.defineProperty(a,'length',{value:100});return JSON.stringify([target(a,0,17),target(a,4,23)===undefined,a.length])})()",
            "(()=>{let shared=new Int32Array(new SharedArrayBuffer(16));return JSON.stringify([target(shared,0,17),shared[0]])})()",
            "(()=>{let buffer=new ArrayBuffer(16,{maxByteLength:32}),view=new Int32Array(buffer,0,4);let first=target(view,0,17);if(typeof buffer.resize!=='function')return JSON.stringify([first,'unsupported']);buffer.resize(0);return JSON.stringify([first,target(view,0,23)===undefined])})()",
        ] {
            let expected = evaluate_without_jit(&source, probe);
            let (_runtime, jit, context) = prepared_typed_store(&source, "target(a,0,3)");
            context.with(|ctx| ctx.eval::<(), _>("a[0]=0")).unwrap();
            let before = jit.metrics();
            let actual = context.with(|ctx| ctx.eval::<String, _>(probe)).unwrap();
            let after = jit.metrics();
            assert_eq!(actual, expected, "{kind}: {probe}");
            assert!(after.tier2_entries > before.tier2_entries, "probe missed Tier2: {kind}: {probe}");
            assert_eq!(after.native_entries, after.native_exits);
        }
    }
}

#[test]
fn typed_store_aliases_and_overflow_keep_completed_effects() {
    let source = "function target(a,b,i,v){a[i]=v;b[i]=b[i]+1;return a[i]+2147483647}";
    let warmup = "target(new Int32Array(4),new Int32Array(4),0,0)";
    let (_runtime, jit, context) = prepared_typed_store(source, warmup);
    let probe = "(()=>{let buffer=new ArrayBuffer(16),a=new Int32Array(buffer),b=new Int32Array(buffer);return JSON.stringify([target(a,b,0,7),a[0],b[0]])})()";
    let before = jit.metrics();
    let actual = context.with(|ctx| ctx.eval::<String, _>(probe)).unwrap();
    assert_eq!(actual, evaluate_without_jit(source, probe));
    assert_eq!(actual, "[2147483655,8,8]");
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn typed_store_loop_revalidates_length_after_observable_poll() {
    typed_store_poll_case(false);
}

#[test]
fn typed_store_loop_revalidates_detach_after_observable_poll() {
    typed_store_poll_case(true);
}

fn typed_store_poll_case(detach: bool) {
    use rquickjs::qjs;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (runtime, jit, context) = prepared_typed_store(
        "function target(a){let last=0;for(let i=0;i<a.length;i++){a[i]=i;last=a[i]}return last}",
        "target(new Int32Array(8))",
    );
    let (raw_ctx, raw_array, raw_buffer) = context.with(|ctx| {
        ctx.eval::<(), _>("globalThis.a=new Int32Array(1000000);globalThis.buffer=a.buffer")
            .unwrap();
        let array: rquickjs::Object = ctx.globals().get("a").unwrap();
        let buffer: rquickjs::Object = ctx.globals().get("buffer").unwrap();
        (
            ctx.as_raw().as_ptr() as usize,
            unsafe { qjs::JS_VALUE_GET_PTR(array.as_value().as_raw()) } as usize,
            unsafe { qjs::JS_VALUE_GET_PTR(buffer.as_value().as_raw()) } as usize,
        )
    });
    let changed = Arc::new(AtomicBool::new(false));
    let observed = changed.clone();
    let failure = Arc::new(AtomicBool::new(false));
    let failed = failure.clone();
    // The globally rooted fixed view remains live. Defining an own primitive
    // length uses the C API without invoking script or reentering the Rust lock.
    runtime.set_interrupt_handler(Some(Box::new(move || unsafe {
        let ctx = raw_ctx as *mut qjs::JSContext;
        let array = qjs::JS_MKPTR(qjs::JS_TAG_OBJECT, raw_array as *mut core::ffi::c_void);
        let value = qjs::JS_GetPropertyUint32(ctx, array, 1);
        let started =
            qjs::JS_VALUE_GET_TAG(value) == qjs::JS_TAG_INT && qjs::JS_VALUE_GET_INT(value) == 1;
        qjs::JS_FreeValue(ctx, value);
        if started && !observed.swap(true, Ordering::SeqCst) {
            if detach {
                let buffer =
                    qjs::JS_MKPTR(qjs::JS_TAG_OBJECT, raw_buffer as *mut core::ffi::c_void);
                qjs::JS_DetachArrayBuffer(ctx, buffer);
            } else if qjs::JS_DefinePropertyValueStr(
                ctx,
                array,
                c"length".as_ptr(),
                qjs::JS_MKVAL(qjs::JS_TAG_INT, 0),
                qjs::JS_PROP_CONFIGURABLE as i32,
            ) < 0
            {
                failed.store(true, Ordering::SeqCst);
                return true;
            }
        }
        false
    })));
    let before = jit.metrics();
    let result = context.with(|ctx| ctx.eval::<i32, _>("target(a)"));
    runtime.set_interrupt_handler(None);
    assert!(!failure.load(Ordering::SeqCst));
    let result = result.unwrap();
    assert!(
        changed.load(Ordering::SeqCst),
        "no poll observed completed stores"
    );
    assert!(
        (1..999999).contains(&result),
        "loop used stale length after callback: {result}"
    );
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn packed_hoist_edge_cases_match_the_same_version_interpreter() {
    const SOURCE: &str = r#"
        function sumPacked(values) {
          let n = values.length, sum = 0;
          for (let i = 0; i < n; i++) sum = (sum + (values[i] | 0)) | 0;
          return sum;
        }
    "#;
    const CASES: &str = r#"JSON.stringify([
        sumPacked([]),
        sumPacked([7]),
        sumPacked([1,,3]),
        sumPacked(new Int32Array([4,5])),
        sumPacked([2147483647,1])
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
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        let before = jit.metrics();
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("sumPacked([1,2,3,4])"))
                .unwrap(),
            10
        );
        let after = jit.metrics();
        if after.tier2_entries > before.tier2_entries && after.pending_worker_jobs == 0 {
            break;
        }
        jit.poll();
        assert!(
            Instant::now() < deadline,
            "packed traversal never reached Tier2: {after:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = jit.metrics();
    let actual = context.with(|ctx| ctx.eval::<String, _>(CASES)).unwrap();
    let after = jit.metrics();
    assert_eq!(actual, expected);
    assert_eq!(actual, "[0,7,4,9,-2147483648]");
    assert_eq!(after.native_entries, after.native_exits);
    assert!(
        after.tier2_entries >= before.tier2_entries + 5,
        "every edge case must enter the prepared Tier2 body: {before:?} -> {after:?}"
    );
    assert!(
        after.deopts >= before.deopts + 2,
        "holes and the wrong ArrayMode must leave the guarded path: {before:?} -> {after:?}"
    );
}

#[test]
fn stale_bound_writes_and_reentry_match_the_same_version_interpreter() {
    const SOURCE: &str = r#"
        function aliasWrite(values) {
          let n = values.length, alias = values, sum = 0;
          for (let i = 0; i < n; i++) {
            if (i === 0) alias.length = 1;
            sum = (sum + (values[i] | 0)) | 0;
          }
          return sum;
        }
        function reentrantWrite(values, mutate) {
          let n = values.length, sum = 0;
          for (let i = 0; i < n; i++) {
            if (i === 0) mutate(values);
            sum = (sum + (values[i] | 0)) | 0;
          }
          return sum;
        }
        function shrink(values) { values.length = 1; }
    "#;
    const CASES: &str = "JSON.stringify([aliasWrite([1,2,3,4]),reentrantWrite([1,2,3,4],shrink)])";
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
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    let mut entered_tier2 = false;
    while Instant::now() < deadline {
        let before = jit.metrics();
        let alias = context
            .with(|ctx| ctx.eval::<i32, _>("aliasWrite([1,2,3,4])"))
            .unwrap();
        assert_eq!(alias, 1, "alias metrics={:?}", jit.metrics());
        let reentry = context
            .with(|ctx| ctx.eval::<i32, _>("reentrantWrite([1,2,3,4],shrink)"))
            .unwrap();
        assert_eq!(reentry, 1, "reentry metrics={:?}", jit.metrics());
        jit.poll();
        if jit.metrics().tier2_entries > before.tier2_entries {
            entered_tier2 = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        entered_tier2,
        "effectful controls never reached Tier2: {:?}",
        jit.metrics()
    );
    let before = jit.metrics();
    let actual = context.with(|ctx| ctx.eval::<String, _>(CASES)).unwrap();
    let after = jit.metrics();
    assert_eq!(actual, expected);
    assert_eq!(actual, "[1,1]");
    assert_eq!(after.native_entries, after.native_exits);
    assert!(after.tier2_entries > before.tier2_entries);
}

#[test]
fn detached_and_resizable_views_match_the_same_version_interpreter() {
    const SOURCE: &str = r#"
        function readZero(values) { return values[0]; }
        function viewCases() {
          const detachedBuffer = new ArrayBuffer(8);
          const detached = new Int32Array(detachedBuffer);
          detached[0] = 17;
          detachedBuffer.transfer();
          const detachedResult = readZero(detached) === undefined;
          let resizableResult = "unsupported";
          try {
            const buffer = new ArrayBuffer(8, {maxByteLength: 16});
            if (typeof buffer.resize === "function") {
              const view = new Int32Array(buffer);
              view[0] = 23;
              buffer.resize(0);
              resizableResult = readZero(view) === undefined;
            }
          } catch (_) {}
          return JSON.stringify([detachedResult, resizableResult]);
        }
    "#;
    let expected = evaluate_without_jit(SOURCE, "viewCases()");

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
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
    let mut entered_tier2 = false;
    while Instant::now() < deadline {
        let before = jit.metrics();
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("readZero(new Int32Array([31]))"))
                .unwrap(),
            31
        );
        jit.poll();
        if jit.metrics().tier2_entries > before.tier2_entries {
            entered_tier2 = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        entered_tier2,
        "typed load never reached Tier2: {:?}",
        jit.metrics()
    );
    let before = jit.metrics();
    let actual = context
        .with(|ctx| ctx.eval::<String, _>("viewCases()"))
        .unwrap();
    let after = jit.metrics();
    assert_eq!(actual, expected);
    assert!(actual == "[true,true]" || actual == "[true,\"unsupported\"]");
    assert_eq!(after.native_entries, after.native_exits);
    assert!(
        after.tier2_entries > before.tier2_entries,
        "detached/resizable controls never entered Tier2: {before:?} -> {after:?}"
    );
    assert!(
        after.deopts > before.deopts,
        "detached storage must fail the native storage guard: {before:?} -> {after:?}"
    );
}
