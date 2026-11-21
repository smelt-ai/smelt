#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
//! Bounded polymorphic (<= 4 shapes) property inline caches in both native
//! tiers, and the megamorphic generic-helper fallback.

use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Runtime};
use rquickjs_jit::{
    bytecode::VerifyLimits,
    compiler::optimized::Tier2Compiler,
    runtime::{
        FeedbackSnapshot, FunctionKey, ObservedType, PropertyAttributes, PrototypeDependencyToken,
        ShapeFeedbackState, ShapeFeedbackTable, ShapeObservation, ShapeToken,
        POLYMORPHIC_PROPERTY_LIMIT,
    },
    test_support::SnapshotFixture,
    Jit, JitConfig, JitTierPolicy,
};

unsafe extern "C" {
    fn JS_JitGetHelperCount(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        helper_id: u32,
        count: *mut u64,
    ) -> i32;
}

fn helper_count(rt: *mut rquickjs_core::qjs::JSRuntime, helper_id: u32) -> u64 {
    let mut count = 0;
    assert_eq!(
        unsafe { JS_JitGetHelperCount(rt, helper_id, &mut count) },
        0
    );
    count
}

fn raw_runtime(context: &Context) -> *mut rquickjs_core::qjs::JSRuntime {
    context.with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) })
}

fn observation(identity: u64, offset: u32, value: ObservedType) -> ShapeObservation {
    ShapeObservation::new(
        ShapeToken::new(identity, 7),
        PrototypeDependencyToken::new(0, 0),
        offset,
        PropertyAttributes::WRITABLE,
        value,
    )
}

fn site_pc(verified: &rquickjs_jit::bytecode::VerifiedFunction, opcode: &str, nth: usize) -> u32 {
    verified
        .instructions()
        .iter()
        .filter(|instruction| instruction.opcode().name() == opcode)
        .nth(nth)
        .unwrap()
        .pc()
}

#[test]
fn shape_feedback_records_four_shapes_before_turning_megamorphic() {
    let mut table = ShapeFeedbackTable::new(POLYMORPHIC_PROPERTY_LIMIT);
    let key = FunctionKey::new(3, 1);
    assert_eq!(POLYMORPHIC_PROPERTY_LIMIT, 4);
    for identity in 1..=4u64 {
        let state = table.observe(
            key,
            9,
            observation(identity * 0x1000, identity as u32, ObservedType::Int32),
        );
        let expected = if identity == 1 {
            ShapeFeedbackState::Monomorphic
        } else {
            ShapeFeedbackState::Polymorphic
        };
        assert_eq!(state, expected);
    }
    assert_eq!(table.get(key, 9).unwrap().observations().len(), 4);
    assert_eq!(
        table.observe(key, 9, observation(0x5000, 5, ObservedType::Int32)),
        ShapeFeedbackState::Megamorphic
    );
    assert!(table.get(key, 9).unwrap().observations().is_empty());
}

fn lower(
    source: &str,
    sites: &[(&str, usize, &[ShapeObservation])],
) -> Result<String, rquickjs_jit::compiler::CompileFailure> {
    let fixture = SnapshotFixture::compile(source);
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(1, 1);
    let mut table = ShapeFeedbackTable::new(POLYMORPHIC_PROPERTY_LIMIT);
    for (opcode, nth, observations) in sites {
        let pc = site_pc(&verified, opcode, *nth);
        for observation in observations.iter() {
            table.observe(key, pc, *observation);
        }
    }
    Tier2Compiler::host(1).lower_with_feedback_for_test(
        &verified,
        key,
        &FeedbackSnapshot::empty(1).with_properties(table.snapshot(key)),
    )
}

#[test]
fn four_shape_tier2_chain_is_inline_without_helper_calls() {
    let four = [
        observation(0x1000, 1, ObservedType::Int32),
        observation(0x2000, 3, ObservedType::Int32),
        observation(0x3000, 2, ObservedType::Int32),
        observation(0x4000, 0, ObservedType::Int32),
    ];
    let source = "(function(o){return o.answer})";
    let mono = lower(source, &[("get_field", 0, &four[..1])]).unwrap();
    let poly = lower(source, &[("get_field", 0, &four)]).unwrap();
    assert_eq!(
        poly.matches("call_indirect").count(),
        mono.matches("call_indirect").count(),
        "{poly}"
    );
    for identity in [0x1000u64, 0x2000, 0x3000, 0x4000] {
        assert!(
            poly.contains(&format!(", {identity}\n"))
                || poly.contains(&format!(", {identity:#x}\n")),
            "missing shape {identity:#x}: {poly}"
        );
    }
}

#[test]
fn megamorphic_tier2_sites_lower_to_generic_helpers_instead_of_guards() {
    let five = (1..=5u64)
        .map(|identity| observation(identity * 0x1000, identity as u32, ObservedType::Int32))
        .collect::<Vec<_>>();
    let get = lower("(function(o){return o.answer})", &[("get_field", 0, &five)])
        .expect("megamorphic get uses the owning helper");
    assert!(get.contains("call_indirect"), "{get}");
    assert!(!get.contains("0x5000") && !get.contains("20480"), "{get}");

    let put = lower(
        "(function(o,v){o.answer=v;return v})",
        &[("put_field", 0, &five)],
    )
    .expect("megamorphic put uses the owning helper");
    assert!(put.contains("call_indirect"), "{put}");
    assert!(!put.contains("0x5000") && !put.contains("20480"), "{put}");
}

#[test]
fn shape_guarded_element_receivers_are_rejected_instead_of_deopting_forever() {
    // Optimized element loads only publish primitives, so a shape guard on an
    // element result can never hit. Such a function must stay in its current
    // tier rather than install an artifact that deopts on every execution.
    let source = "(function(a,i){return a[i].answer})";
    let four = [
        observation(0x1000, 1, ObservedType::Int32),
        observation(0x2000, 3, ObservedType::Int32),
    ];
    assert_eq!(
        lower(source, &[("get_field", 0, &four)]).unwrap_err(),
        rquickjs_jit::compiler::CompileFailure::UnsupportedOpcode
    );
    assert_eq!(
        lower(source, &[("get_field", 0, &four[..1])]).unwrap_err(),
        rquickjs_jit::compiler::CompileFailure::UnsupportedOpcode
    );
    // A generic site accepts any receiver and remains admissible.
    let five = (1..=5u64)
        .map(|identity| observation(identity * 0x1000, identity as u32, ObservedType::Int32))
        .collect::<Vec<_>>();
    assert!(lower(source, &[("get_field", 0, &five)]).is_ok());
}

#[test]
fn production_element_receiver_kernel_never_enters_a_deopt_loop() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "{SHAPES}\n\
                 function sweep(list,n){{let s=0;for(let i=0;i<n;i++){{const o=list[i&3];s=s+o.x+o.y}}return s}}"
            ))
        })
        .unwrap();
    for _ in 0..400 {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("sweep(shapes,64)"))
                .unwrap(),
            16 * 110
        );
        jit.poll();
    }
    let metrics = jit.metrics();
    assert_eq!(metrics.deopts, 0, "{metrics:?}");
    assert_eq!(metrics.tier2_entries, 0, "{metrics:?}");
}

fn wait_for(jit: &Jit, mut step: impl FnMut(), done: impl Fn(&rquickjs_jit::JitMetrics) -> bool) {
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        step();
        jit.poll();
        if done(&jit.metrics()) {
            return;
        }
        assert!(Instant::now() < deadline, "{:?}", jit.metrics());
        std::thread::sleep(Duration::from_micros(50));
    }
}

const SHAPES: &str = r#"
    globalThis.shapes = [
        { x: 1, y: 10 },
        { y: 20, x: 2, z: 0 },
        { tag: 0, x: 3, y: 30 },
        Object.assign(Object.create(null), { y: 40, w: 0, x: 4 }),
    ];
    globalThis.extra = [
        { q: 0, r: 0, x: 5, y: 50 },
        { s: 0, x: 6, y: 60 },
    ];
"#;

#[test]
fn production_tier2_four_shape_site_runs_without_deopt_under_stress_gc() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(64)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "{SHAPES}\nfunction bump(o,v){{o.x=o.x+v;o.x=o.x-v;return o.x+o.y}}"
            ))
        })
        .unwrap();
    let call = |index: usize| {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("bump").unwrap();
            let shapes: rquickjs::Array = ctx.globals().get("shapes").unwrap();
            let o: Object = shapes.get(index & 3).unwrap();
            let result = f.call::<_, i32>((o, 3));
            assert!(result.is_ok(), "{result:?}; catch={:?}", ctx.catch());
            assert_eq!(result.unwrap(), 11 * ((index & 3) as i32 + 1));
        })
    };
    let mut index = 0;
    wait_for(
        &jit,
        || {
            call(index);
            index += 1;
        },
        |metrics| metrics.tier2_entries > 0,
    );
    let rt = raw_runtime(&context);
    let before_shape = helper_count(
        rt,
        rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SHAPE_GUARD,
    );
    let before = jit.metrics();
    for index in 0..1_024 {
        call(index);
        jit.poll();
    }
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert_eq!(after.deopts, before.deopts, "{before:?} -> {after:?}");
    assert_eq!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SHAPE_GUARD
        ),
        before_shape
    );
}

#[test]
fn production_tier2_megamorphic_site_uses_helpers_with_exact_reentrancy() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(64)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "{SHAPES}\n\
                 globalThis.all = shapes.concat(extra);\n\
                 globalThis.getterCalls = 0;\n\
                 globalThis.accessor = {{\n\
                   get x() {{ getterCalls++; return 7; }},\n\
                   set x(v) {{ if (v === 1000) throw new Error('setter'); this.stored = v; }},\n\
                   y: 70\n\
                 }};\n\
                 function mega(o,v){{o.x=v;return o.x+o.y}}\n\
                 function guarded(o,v){{try{{return mega(o,v)}}catch(e){{return -1}}}}"
            ))
        })
        .unwrap();
    let call = |index: usize| {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("mega").unwrap();
            let all: rquickjs::Array = ctx.globals().get("all").unwrap();
            let o: Object = all.get(index % 6).unwrap();
            let result = f.call::<_, i32>((o, 1));
            assert!(result.is_ok(), "{result:?}; catch={:?}", ctx.catch());
            assert_eq!(result.unwrap(), 1 + 10 * ((index % 6) as i32 + 1));
        })
    };
    let mut index = 0;
    wait_for(
        &jit,
        || {
            call(index);
            index += 1;
        },
        |metrics| metrics.tier2_entries > 0,
    );
    let before = jit.metrics();
    for index in 0..600 {
        call(index);
        jit.poll();
    }
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert_eq!(
        after.deopts, before.deopts,
        "a megamorphic site must not guard or deopt on a shape: {before:?} -> {after:?}"
    );
    // Accessor receivers take the reentrant generic path: the getter runs,
    // and a throwing setter propagates through the owned exception exit.
    let [value, thrown, calls] = context
        .with(|ctx| {
            ctx.eval::<Vec<i32>, _>(
                "(() => { const a = mega(accessor, 5); const b = guarded(accessor, 1000);\
                  return [a, b, getterCalls]; })()",
            )
        })
        .unwrap()[..]
    else {
        panic!("three results");
    };
    assert_eq!((value, thrown), (77, -1));
    assert!(calls >= 1);
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("accessor.stored"))
            .unwrap(),
        5
    );
    for index in 0..64 {
        call(index);
    }
    runtime.run_gc();
}

/// Integration of the megamorphic Tier-2 store (P4c) with inline reference
/// counting (P1b): without stress GC the receiver owner is released inline
/// when it is shared (an argument receiver the ownership bridge duplicated)
/// and through FREE when the site holds the last reference (a call result).
/// Neither path may leak or double-free the receiver.
#[test]
fn production_tier2_megamorphic_store_releases_receivers_inline_without_leaks() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(64)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "{SHAPES}\n\
                 globalThis.all = shapes.concat(extra);\n\
                 globalThis.made = 0;\n\
                 function make(i){{made++;switch(i%6){{\
                   case 0:return {{x:0,y:1}};case 1:return {{y:2,x:0}};\
                   case 2:return {{a:0,x:0}};case 3:return {{b:0,x:0}};\
                   case 4:return {{c:0,x:0}};default:return {{d:0,e:0,x:0}}}}}}\n\
                 function store(o,i,v){{o.x=v;make(i).x=v;return o.x+i}}"
            ))
        })
        .unwrap();
    let call = |index: usize| {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("store").unwrap();
            let all: rquickjs::Array = ctx.globals().get("all").unwrap();
            let o: Object = all.get(index % 6).unwrap();
            let value = (index % 97) as i32;
            let result = f.call::<_, i32>((o, index as i32, value));
            assert!(result.is_ok(), "{result:?}; catch={:?}", ctx.catch());
            assert_eq!(result.unwrap(), value + index as i32);
        })
    };
    let mut index = 0;
    wait_for(
        &jit,
        || {
            call(index);
            index += 1;
        },
        |metrics| metrics.tier2_entries > 0,
    );
    runtime.run_gc();
    let objects_before = runtime.memory_usage().obj_count;
    let before = jit.metrics();
    for index in 0..600 {
        call(index);
        jit.poll();
    }
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert_eq!(after.deopts, before.deopts, "{before:?} -> {after:?}");
    runtime.run_gc();
    assert_eq!(
        runtime.memory_usage().obj_count,
        objects_before,
        "a megamorphic store leaked (or freed) its receiver"
    );
    // Every argument receiver is still alive and holds the last stored value.
    let stored = context
        .with(|ctx| ctx.eval::<Vec<i32>, _>("all.map((o) => o.x)"))
        .unwrap();
    for (slot, value) in stored.into_iter().enumerate() {
        let last = (594 + slot) % 97;
        assert_eq!(value, last as i32, "receiver {slot}");
    }
    assert_eq!(after.native_entries, after.native_exits);
}

#[test]
fn baseline_inline_dispatch_covers_four_shapes_without_helpers() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(2)
            .loop_threshold(2)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "{SHAPES}\n\
                 function sweep(list,n){{let s=0;for(let i=0;i<n;i++){{const o=list[i&3];o.x=o.x+1;s=s+o.x+o.y;o.x=o.x-1}}return s}}"
            ))
        })
        .unwrap();
    let run =
        |n: i32| context.with(|ctx| ctx.eval::<i32, _>(format!("sweep(shapes,{n})")).unwrap());
    // Each group of four receivers contributes (2+10)+(3+20)+(4+30)+(5+40).
    wait_for(
        &jit,
        || assert_eq!(run(8), 2 * 114),
        |metrics| metrics.installed >= 2,
    );
    let rt = raw_runtime(&context);
    assert_eq!(
        unsafe { rquickjs_core::qjs::JS_JitResetHelperCounters(rt) },
        0
    );
    assert_eq!(run(64), 16 * 114);
    for helper in [
        rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SHAPE_GUARD,
        rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY,
        rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_PROPERTY,
    ] {
        assert_eq!(helper_count(rt, helper), 0, "helper {helper} on a hit");
    }
    // An unobserved fifth layout misses every compare and takes the exact
    // generic helper edge; a mutated observed layout does too.
    let fifth = context
        .with(|ctx| ctx.eval::<i32, _>("sweep([extra[0],extra[0],extra[0],extra[0]],4)"))
        .unwrap();
    assert_eq!(fifth, 4 * 56);
    assert!(
        helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY
        ) > 0
    );
    let mutated = context
        .with(|ctx| ctx.eval::<i32, _>("delete shapes[1].z; sweep(shapes,4)"))
        .unwrap();
    assert_eq!(mutated, 114);
    assert!(jit.metrics().native_entries > 0);
}

#[test]
fn automatic_tiering_refreshes_a_terminal_baseline_with_property_ics() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(2)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    // The object literal is outside the Tier-2 vocabulary, so the baseline
    // artifact is this function's terminal native tier.
    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "{SHAPES}\n\
                 function sweep(list,n){{const box={{s:0}};for(let i=0;i<n;i++){{const o=list[i&3];box.s=box.s+o.x+o.y}}return box.s}}"
            ))
        })
        .unwrap();
    let run =
        |n: i32| context.with(|ctx| ctx.eval::<i32, _>(format!("sweep(shapes,{n})")).unwrap());
    wait_for(
        &jit,
        || assert_eq!(run(8), 2 * 110),
        |metrics| metrics.installed >= 2 && metrics.pending_worker_jobs == 0,
    );
    let rt = raw_runtime(&context);
    let settle =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    loop {
        assert_eq!(
            unsafe { rquickjs_core::qjs::JS_JitResetHelperCounters(rt) },
            0
        );
        assert_eq!(run(64), 16 * 110);
        if helper_count(
            rt,
            rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY,
        ) == 0
        {
            break;
        }
        jit.poll();
        assert!(Instant::now() < settle, "{:?}", jit.metrics());
        std::thread::sleep(Duration::from_micros(50));
    }
    assert_eq!(jit.metrics().tier2_entries, 0, "{:?}", jit.metrics());
    assert!(jit.metrics().native_entries > 0, "{:?}", jit.metrics());
}
