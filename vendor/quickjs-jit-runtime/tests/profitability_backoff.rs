#![cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use std::{thread, time::Duration};

use rquickjs::{Context, Function, Object, Runtime};
use rquickjs_jit::{
    bytecode::{CompileSnapshot, VerifyLimits},
    compiler::{optimized::Tier2CompileStage, CompileFailure},
    ir::{FrameSlot, OptimizedIr, OptimizedNodeKind, ScalarValue, ScalarValueId},
    runtime::FunctionKey,
    Jit, JitConfig,
};

fn automatic_runtime(stress_gc: bool) -> (Runtime, Jit, Context) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .stress_gc(stress_gc)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    (runtime, jit, context)
}

fn assert_object_target_caller_reaches_tier2(body: &str) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(format!(
            r#"
                function ownedTarget(value, state) {{
                    state.calls = state.calls + 1;
                    return value + 1;
                }}
                globalThis.ownershipState = {{
                    target: ownedTarget,
                    left: ownedTarget,
                    right: ownedTarget,
                    holder: {{ method: ownedTarget }},
                    flag: true,
                    calls: 0
                }};
                globalThis.ownershipCaller = function(iterations, state) {{
                    state.calls = 0;
                    {body}
                    return state.calls;
                }};
                "#
        ))
        .unwrap();
    });
    for iteration in 0..10_000 {
        context.with(|ctx| {
            let state: Object = ctx.globals().get("ownershipState").unwrap();
            state.set("flag", iteration & 1 == 0).unwrap();
            let caller: Function = ctx.globals().get("ownershipCaller").unwrap();
            assert_eq!(caller.call::<_, i32>((32, state)).unwrap(), 32);
        });
        jit.poll();
        let before = jit.metrics().tier2_entries;
        context.with(|ctx| {
            let state: Object = ctx.globals().get("ownershipState").unwrap();
            let caller: Function = ctx.globals().get("ownershipCaller").unwrap();
            assert_eq!(caller.call::<_, i32>((0, state)).unwrap(), 0);
        });
        jit.poll();
        if jit.metrics().tier2_entries > before {
            runtime.run_gc();
            return;
        }
        thread::sleep(Duration::from_micros(50));
    }
    panic!(
        "object-target caller did not reach Tier2: {:?}; compile={:?}",
        jit.metrics(),
        jit.test_tier2_compile_dispositions()
    );
}

fn function_snapshot(context: &Context, name: &str) -> CompileSnapshot {
    context.with(|ctx| {
        let function: Function = ctx.globals().get(name).unwrap();
        unsafe { CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_value().as_raw()) }
            .unwrap()
    })
}

#[test]
fn compilation_diagnostics_do_not_cross_runtime_boundaries() {
    let make_runtime = || {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .call_threshold(1)
                .force_optimized_for_test(true)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            ctx.eval::<(), _>("globalThis.isolationTarget = function(x) { return x + 1; };")
                .unwrap();
        });
        let snapshot = function_snapshot(&context, "isolationTarget");
        let key = FunctionKey::new(snapshot.function_id(), snapshot.generation());
        (runtime, jit, context, key)
    };
    let (_first_runtime, first, context, first_key) = make_runtime();
    let (_second_runtime, second, _second_context, second_key) = make_runtime();
    assert_eq!(
        first_key, second_key,
        "fixture must exercise colliding function IDs"
    );
    for _ in 0..10_000 {
        context.with(|ctx| {
            let target: Function = ctx.globals().get("isolationTarget").unwrap();
            assert_eq!(target.call::<_, i32>((41,)).unwrap(), 42);
        });
        first.poll();
        if first.metrics().tier2_entries != 0 {
            break;
        }
        thread::sleep(Duration::from_micros(50));
    }
    assert_ne!(first.metrics().tier2_entries, 0);
    assert!(!first.test_tier2_compile_dispositions().is_empty());
    assert!(!first.test_tier2_deopt_sites().is_empty());
    assert!(second.test_tier2_compile_dispositions().is_empty());
    assert!(second.test_tier2_deopt_sites().is_empty());
}

fn scalar_value_reaches_argument(
    values: &[ScalarValue],
    start: ScalarValueId,
    argument: u16,
) -> bool {
    let mut pending = vec![start];
    let mut visited = std::collections::BTreeSet::new();
    while let Some(value) = pending.pop() {
        if !visited.insert(value.index()) {
            continue;
        }
        match &values[value.index()] {
            ScalarValue::Input {
                slot: FrameSlot::Argument(index),
                ..
            }
            | ScalarValue::FrameRead {
                slot: FrameSlot::Argument(index),
                ..
            } if *index == argument => return true,
            ScalarValue::Phi { inputs, .. } => {
                pending.extend(inputs.iter().map(|input| input.value));
            }
            _ => {}
        }
    }
    false
}

#[test]
fn generic_call_fails_closed_for_a_mixed_unknown_phi_inside_a_loop() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(16)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let effect = Function::new(ctx.clone(), |state: Object<'_>| -> rquickjs::Result<i32> {
            let calls: i32 = state.get("calls")?;
            state.set("calls", calls + 1)?;
            Ok(1)
        })
        .unwrap();
        ctx.globals().set("mixedPhiEffect", effect).unwrap();
        ctx.eval::<(), _>(
            r#"
                globalThis.mixedPhiState = { value: 100, calls: 0 };
                globalThis.mixedPhiCaller = function(f, useArgument, argument, state, iterations) {
                    let total = 0;
                    for (let i = 0; i < iterations; i++) {
                        // The conditional creates a real stack Phi. Its first
                        // edge borrows `argument`; its second edge is a
                        // GetProperty result with no proven provenance. The
                        // Phi remains live across the generic host call on
                        // every loop backedge.
                        total += (useArgument ? argument : state.value) + f(state);
                    }
                    return total;
                };
                "#,
        )
        .unwrap();
    });
    let snapshot = function_snapshot(&context, "mixedPhiCaller");
    let key = FunctionKey::new(snapshot.function_id(), snapshot.generation());
    let verified = snapshot.verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 1).unwrap();
    assert!(ir.blocks().iter().any(|block| block.is_loop_header()));
    let call = ir
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.starts_with("call"))
        })
        .expect("generic host call in the loop body");
    let graph = ir.scalar_graph();
    let state = graph
        .frame_state_for_node(call.id())
        .expect("pre-call frame state");
    let semantic_call = graph.call(call.id()).expect("semantic call operands");
    let live_prefix = &state.stack[..state.stack.len() - semantic_call.arguments.len() - 1];
    assert!(
        live_prefix.iter().any(|value| {
            let ScalarValue::Phi { inputs, .. } = &graph.values()[value.index()] else {
                return false;
            };
            inputs.iter().any(|input| {
                matches!(
                    graph.values()[input.value.index()],
                    ScalarValue::GetProperty { .. }
                )
            }) && inputs
                .iter()
                .any(|input| scalar_value_reaches_argument(graph.values(), input.value, 2))
        }),
        "the CALL live prefix lacks the expected Argument/GetProperty stack Phi: {state:#?}\n{:#?}",
        graph.values()
    );
    let invoke = |use_argument: bool| {
        context.with(|ctx| {
            let caller: Function = ctx.globals().get("mixedPhiCaller").unwrap();
            let effect: Function = ctx.globals().get("mixedPhiEffect").unwrap();
            let state: Object = ctx.globals().get("mixedPhiState").unwrap();
            caller
                .call::<_, i32>((effect, use_argument, 41, state, 3))
                .unwrap()
        })
    };

    let deadline = std::time::Instant::now()
        + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    let mut disposition = None;
    let mut invocations = 0;
    for iteration in 0..10_000 {
        assert_eq!(
            invoke(iteration & 1 == 0),
            if iteration & 1 == 0 { 126 } else { 303 }
        );
        invocations += 1;
        jit.poll();
        disposition = jit
            .test_tier2_compile_dispositions()
            .into_iter()
            .find_map(|(candidate, disposition)| (candidate == key).then_some(disposition));
        if disposition.is_some_and(|disposition| {
            matches!(
                disposition.stage,
                Tier2CompileStage::GenericCallPrefixUnknown | Tier2CompileStage::Complete
            )
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "mixed-Phi caller did not reach a terminal Tier-2 disposition: metrics={:?}, dispositions={:?}",
            jit.metrics(),
            jit.test_tier2_compile_dispositions()
        );
        thread::sleep(Duration::from_micros(50));
    }

    let disposition = disposition.expect("mixed-Phi caller was considered for Tier 2");
    assert_eq!(
        disposition.stage,
        Tier2CompileStage::GenericCallPrefixUnknown,
        "a borrowed/owned live-prefix Phi must fail closed: {disposition:?}"
    );
    assert_eq!(disposition.failure, Some(CompileFailure::InvalidArtifact));
    assert_eq!(jit.metrics().tier2_entries, 0, "{:?}", jit.metrics());

    // Rejection must leave the baseline path exact while stress GC exercises
    // the frame's ownership transitions through the effectful host call.
    for iteration in 0..64 {
        assert_eq!(
            invoke(iteration & 1 == 0),
            if iteration & 1 == 0 { 126 } else { 303 }
        );
        runtime.run_gc();
    }
    assert_eq!(jit.metrics().tier2_entries, 0, "{:?}", jit.metrics());
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("mixedPhiState.calls").unwrap()),
        3 * (64 + invocations)
    );
}

#[test]
fn borrowed_property_result_moves_into_an_owned_local() {
    assert_object_target_caller_reaches_tier2(
        "const target = state.target; for (let i=0;i<iterations;i++) target(i,state);",
    );
}

#[test]
fn owned_slot_local_survives_an_unrelated_primitive_local() {
    assert_object_target_caller_reaches_tier2(
        "const target=state.target; const limit=iterations; for(let i=0;i<limit;i++)target(i,state);",
    );
}

#[test]
fn call_method_keeps_the_owned_receiver_and_target_live() {
    assert_object_target_caller_reaches_tier2(
        "for(let i=0;i<iterations;i++) state.holder.method(i,state);",
    );
}

#[test]
fn owned_property_local_survives_a_control_flow_diamond() {
    assert_object_target_caller_reaches_tier2(
        "const target=state.target; if(state.flag)state.calls=0;else state.calls=0;for(let i=0;i<iterations;i++)target(i,state);",
    );
}

#[test]
fn owned_property_local_survives_each_backedge() {
    assert_object_target_caller_reaches_tier2(
        "const holder=state.holder;for(let i=0;i<iterations;i++)holder.method(i,state);",
    );
}

#[test]
fn effectful_call_link_does_not_stall_caller_at_generic_baseline() {
    let (_runtime, jit, context) = automatic_runtime(true);
    context.with(|ctx| {
        ctx.eval::<(), _>(
            r#"
                function incrementAndRecord(value, enabled, state) {
                    state.calls = state.calls + 1;
                    if (enabled) return value + 1;
                    return value;
                }
                function genericLoop(iterations, seed, state) {
                    state.calls = 0;
                    const target = state.target;
                    let value = seed;
                    // The leading addition operand is a live tagged prefix
                    // across CALL. Tier 2 must preserve it while the generic
                    // bridge consumes only target/arguments.
                    for (let i = 0; i < iterations; i++)
                        value = 1 + target(value, true, state) - 1;
                    return value + state.calls;
                }
                function incrementTwiceAndRecord(value, enabled, state) {
                    state.calls = state.calls + 1;
                    if (enabled) return value + 2;
                    return value;
                }
                globalThis.genericState = { target: incrementAndRecord, calls: 0 };
                "#,
        )
        .unwrap();
    });

    let mut caller_entered_tier2 = false;
    for _ in 0..2_000 {
        context.with(|ctx| {
            let state: Object = ctx.globals().get("genericState").unwrap();
            let generic: Function = ctx.globals().get("genericLoop").unwrap();
            assert_eq!(generic.call::<_, i32>((2_000, 1, state)).unwrap(), 4_001);
        });
        jit.poll();
        // A zero-trip probe cannot enter the callee. Its Tier-2 delta therefore
        // proves that the caller itself progressed beyond the generic baseline.
        let before = jit.metrics().tier2_entries;
        context.with(|ctx| {
            let state: Object = ctx.globals().get("genericState").unwrap();
            let generic: Function = ctx.globals().get("genericLoop").unwrap();
            assert_eq!(generic.call::<_, i32>((0, 1, state)).unwrap(), 1);
        });
        jit.poll();
        if jit.metrics().tier2_entries > before {
            caller_entered_tier2 = true;
            break;
        }
        thread::sleep(Duration::from_micros(50));
    }
    assert!(
        caller_entered_tier2,
        "metrics={:?}, completions={:?}, tier2_compile={:?}",
        jit.metrics(),
        jit.test_completion_dispositions(),
        jit.test_tier2_compile_dispositions()
    );
    assert_eq!(
        jit.metrics().generic_call_rejections,
        0,
        "{:?}",
        jit.metrics()
    );

    let before = jit.metrics().tier2_entries;
    for _ in 0..20 {
        context.with(|ctx| {
            let state: Object = ctx.globals().get("genericState").unwrap();
            let generic: Function = ctx.globals().get("genericLoop").unwrap();
            assert_eq!(generic.call::<_, i32>((0, 1, state)).unwrap(), 1);
        });
        jit.poll();
    }
    assert!(
        jit.metrics().tier2_entries >= before + 20,
        "metrics={:?}, guards={:?}, sites={:?}",
        jit.metrics(),
        jit.test_last_deopt_guards(),
        jit.test_tier2_deopt_sites()
    );

    // The property helper moves an owned function object into the captured
    // local. Stress GC plus replacement exercises both the local root and the
    // old-target release path; the changed identity may deopt, but must remain
    // semantically exact and must not leave a duplicate stack owner behind.
    context
        .with(|ctx| {
            ctx.eval::<(), _>("genericState.target = incrementTwiceAndRecord")?;
            let state: Object = ctx.globals().get("genericState")?;
            let generic: Function = ctx.globals().get("genericLoop")?;
            assert_eq!(generic.call::<_, i32>((2, 1, state))?, 7);
            Ok::<_, rquickjs::Error>(())
        })
        .unwrap();
    jit.poll();
}

#[test]
fn measured_profitable_tier2_remains_published() {
    let (_runtime, jit, context) = automatic_runtime(false);
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function numericLoop(iterations,seed){let n=seed;for(let i=0;i<iterations;i++)n=n+i;return n}",
            )
            .unwrap();
        });

    for _ in 0..20_000 {
        let value = context.with(|ctx| {
            let numeric: Function = ctx.globals().get("numericLoop").unwrap();
            numeric.call::<_, i32>((2_000, 1)).unwrap()
        });
        assert_eq!(value, 1_999_001);
        jit.poll();
        if jit.metrics().tier2_entries >= 16 {
            break;
        }
        thread::sleep(Duration::from_micros(50));
    }
    let before = jit.metrics();
    assert!(before.tier2_entries >= 16, "{before:?}");
    for _ in 0..32 {
        let value = context.with(|ctx| {
            let numeric: Function = ctx.globals().get("numericLoop").unwrap();
            numeric.call::<_, i32>((2_000, 1)).unwrap()
        });
        assert_eq!(value, 1_999_001);
        jit.poll();
    }
    let after = jit.metrics();
    assert_eq!(
        after.optimized_demotions, before.optimized_demotions,
        "{after:?}"
    );
    assert!(
        after.tier2_entries >= before.tier2_entries + 32,
        "{after:?}"
    );
}
