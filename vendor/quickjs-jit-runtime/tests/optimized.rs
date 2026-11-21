use rquickjs_jit::bytecode::{CompileSnapshot, VerifyLimits};
use rquickjs_jit::code_cache::CompiledArtifact;
use rquickjs_jit::compiler::optimized::{
    NumericBinaryOp, OptimizedCompiler, OptimizedInput, Tier2Compiler,
};
use rquickjs_jit::ir::{OptimizedEffect, OptimizedIr, OptimizedNodeKind, ValueRepresentation};
use rquickjs_jit::runtime::{
    ArrayAccess, ArrayHazards, ArrayMode, BinaryFeedbackFlags, CompileCompletion, Coordinator,
    DependencyGraph, DependencyKey, FeedbackKind, FeedbackSnapshot, FeedbackState, FeedbackTable,
    FunctionKey, ObservedType, SideExitAction, Tier,
};
use rquickjs_jit::test_support::SnapshotFixture;

#[test]
fn feedback_is_bounded_and_transitions_monotonically() {
    let function = FunctionKey::new(7, 3);
    let mut table = FeedbackTable::new(2, 2);

    assert_eq!(
        table.observe_type(function, 11, FeedbackKind::Value, ObservedType::Int32),
        FeedbackState::Monomorphic
    );
    assert_eq!(
        table.observe_type(function, 11, FeedbackKind::Value, ObservedType::Float64),
        FeedbackState::Polymorphic
    );
    assert_eq!(
        table.observe_type(function, 11, FeedbackKind::Value, ObservedType::String),
        FeedbackState::Megamorphic
    );
    assert_eq!(
        table.observe_type(function, 11, FeedbackKind::Value, ObservedType::Int32),
        FeedbackState::Megamorphic
    );

    table.observe_type(function, 12, FeedbackKind::Value, ObservedType::Int32);
    table.observe_type(function, 13, FeedbackKind::Value, ObservedType::Int32);
    assert_eq!(table.len(), 2);
    assert_eq!(table.dropped_observations(), 1);
}

#[test]
fn repeated_feedback_preserves_widening_generations_and_capacity_accounting() {
    let a = FunctionKey::new(7, 1);
    let next = FunctionKey::new(7, 2);
    let mut table = FeedbackTable::new(2, 3);
    for _ in 0..8 {
        assert_eq!(
            table.observe_type(a, 1, FeedbackKind::Value, ObservedType::Int32),
            FeedbackState::Monomorphic
        );
        assert_eq!(
            table.observe_type(next, 1, FeedbackKind::Value, ObservedType::Bool),
            FeedbackState::Monomorphic
        );
    }
    assert_eq!(
        table.observe_type(a, 1, FeedbackKind::Value, ObservedType::Float64),
        FeedbackState::Polymorphic
    );
    assert_eq!(
        table.observe_type(a, 1, FeedbackKind::Value, ObservedType::Int32),
        FeedbackState::Polymorphic
    );
    let version = table.version();
    for _ in 0..8 {
        assert_eq!(
            table.observe_type(a, 1, FeedbackKind::Value, ObservedType::Int32),
            FeedbackState::Polymorphic
        );
        assert_eq!(
            table.observe_type(next, 1, FeedbackKind::Value, ObservedType::Bool),
            FeedbackState::Monomorphic
        );
        assert_eq!(
            table.observe_type(a, 2, FeedbackKind::Value, ObservedType::Int32),
            FeedbackState::Megamorphic
        );
    }
    assert_eq!(table.version(), version);
    assert_eq!(
        table.dropped_observations(),
        8,
        "repeated rejected observations still count"
    );
    table.observe_call(a, &[ObservedType::Int32]);
    let version = table.version();
    for _ in 0..8 {
        table.observe_call(a, &[ObservedType::Int32]);
    }
    assert_eq!(table.version(), version);
    table.observe_call(a, &[ObservedType::Int32, ObservedType::Bool]);
    table.observe_call(a, &[ObservedType::Float64]);
    table.observe_call(a, &[ObservedType::Int32]);
    table.observe_call(next, &[ObservedType::Bool]);
    let snapshot = table.snapshot(1);
    let call = snapshot.call_at(a).unwrap();
    assert_eq!(
        call.argument(0),
        [ObservedType::Int32, ObservedType::Float64]
    );
    assert_eq!(call.argument(1), [ObservedType::Bool]);
    assert_eq!(
        snapshot.call_at(next).unwrap().argument(0),
        [ObservedType::Bool]
    );
}

#[test]
fn feedback_models_all_arguments_return_sites_and_binary_operands() {
    let function = FunctionKey::new(17, 9);
    let mut table = FeedbackTable::new(32, 3);
    table.observe_call(
        function,
        &[
            ObservedType::Int32,
            ObservedType::Float64,
            ObservedType::String,
        ],
    );
    table.observe_return(function, 41, ObservedType::Int32);
    table.observe_return(function, 57, ObservedType::String);
    table.observe_binary(
        function,
        23,
        ObservedType::Int32,
        ObservedType::Int32,
        ObservedType::Float64,
        BinaryFeedbackFlags::OVERFLOW,
    );

    let snapshot = table.snapshot(11);
    assert_eq!(
        snapshot.call_argument_types(function),
        Some(
            &[
                ObservedType::Int32,
                ObservedType::Float64,
                ObservedType::String,
            ][..]
        )
    );
    assert_eq!(
        snapshot.stable_return_at(function, 41),
        Some(ObservedType::Int32)
    );
    assert_eq!(
        snapshot.stable_return_at(function, 57),
        Some(ObservedType::String)
    );
    let binary = snapshot.binary_at(function, 23).expect("binary slot");
    assert_eq!(binary.lhs(), &[ObservedType::Int32]);
    assert_eq!(binary.rhs(), &[ObservedType::Int32]);
    assert_eq!(binary.result(), &[ObservedType::Float64]);
    assert!(binary.flags().contains(BinaryFeedbackFlags::OVERFLOW));
}

#[test]
fn feedback_lattice_only_widens_and_flags_accumulate() {
    let function = FunctionKey::new(23, 4);
    let mut table = FeedbackTable::new(32, 2);
    table.observe_binary(
        function,
        7,
        ObservedType::Int32,
        ObservedType::Int32,
        ObservedType::Int32,
        BinaryFeedbackFlags::NONE,
    );
    table.observe_binary(
        function,
        7,
        ObservedType::Float64,
        ObservedType::Int32,
        ObservedType::Float64,
        BinaryFeedbackFlags::NEGATIVE_ZERO,
    );
    table.observe_binary(
        function,
        7,
        ObservedType::String,
        ObservedType::Object,
        ObservedType::String,
        BinaryFeedbackFlags::NAN,
    );
    table.observe_binary(
        function,
        7,
        ObservedType::Int32,
        ObservedType::Int32,
        ObservedType::Int32,
        BinaryFeedbackFlags::NONE,
    );

    let snapshot = table.snapshot(12);
    let binary = snapshot.binary_at(function, 7).unwrap();
    assert_eq!(binary.state(), FeedbackState::Megamorphic);
    assert!(binary.flags().contains(BinaryFeedbackFlags::NEGATIVE_ZERO));
    assert!(binary.flags().contains(BinaryFeedbackFlags::NAN));
    assert_eq!(snapshot.function(), Some(function));
    assert_eq!(snapshot.epoch(), 12);
    assert!(snapshot.binary_at(FunctionKey::new(23, 5), 7).is_none());
}

#[test]
fn alternating_argument_types_widen_each_call_slot_without_shifting_positions() {
    let function = FunctionKey::new(31, 2);
    let mut table = FeedbackTable::new(32, 2);
    table.observe_call(function, &[ObservedType::Int32, ObservedType::String]);
    table.observe_call(function, &[ObservedType::Float64, ObservedType::String]);
    table.observe_call(function, &[ObservedType::Object, ObservedType::String]);

    let snapshot = table.snapshot(19);
    let call = snapshot.call_at(function).unwrap();
    assert_eq!(call.state(), FeedbackState::Megamorphic);
    assert_eq!(call.argument(0), &[]);
    assert_eq!(call.argument(1), &[ObservedType::String]);
    assert_eq!(call.argc(), 2);
    assert_eq!(snapshot.call_argument_types(function), None);
}

#[test]
fn compile_request_carries_the_runtime_epoch_to_the_worker() {
    let fixture = SnapshotFixture::compile("(function(){return 1})");
    let snapshot = fixture.snapshot();
    let key = FunctionKey::new(snapshot.function_id(), snapshot.generation());
    let verified = snapshot.verify(VerifyLimits::default()).unwrap();
    let mut coordinator = Coordinator::with_limits(2, 2, 2, 1 << 20);
    coordinator.advance_clock(77);
    coordinator.queue(key, Tier::Baseline, verified).unwrap();
    assert_eq!(coordinator.begin_next().unwrap().feedback_epoch(), 77);
}

#[test]
fn compile_request_owns_an_immutable_feedback_snapshot() {
    let fixture = SnapshotFixture::compile("(function(n){return n+1})");
    let snapshot = fixture.snapshot();
    let key = FunctionKey::new(snapshot.function_id(), snapshot.generation());
    let verified = snapshot.verify(VerifyLimits::default()).unwrap();
    let mut feedback = FeedbackTable::new(8, 2);
    feedback.observe_type(key, 0, FeedbackKind::Value, ObservedType::Int32);
    let frozen = feedback.snapshot(9);
    let mut coordinator = Coordinator::with_limits(2, 2, 2, 1 << 20);

    coordinator
        .queue_with_feedback(key, Tier::Baseline, verified, frozen)
        .unwrap();
    feedback.observe_type(key, 0, FeedbackKind::Value, ObservedType::String);

    let request = coordinator.begin_next().unwrap();
    assert_eq!(request.feedback().epoch(), 9);
    assert_eq!(
        request.feedback().entries()[0].observations(),
        &[ObservedType::Int32]
    );
}

#[test]
fn tier2_feedback_requires_exact_generation_stable_value_and_nonzero_epoch() {
    let key = FunctionKey::new(41, 7);
    let other_generation = FunctionKey::new(41, 8);
    let mut table = FeedbackTable::new(8, 2);
    table.observe_type(key, 0, FeedbackKind::Value, ObservedType::Int32);
    assert!(!table.snapshot(0).has_stable_value_for(key));
    assert!(!table.snapshot(1).has_stable_value_for(other_generation));
    assert!(table.snapshot(1).has_stable_value_for(key));
    table.observe_type(key, 0, FeedbackKind::Value, ObservedType::String);
    assert!(!table.snapshot(2).has_stable_value_for(key));
}

#[test]
fn narrow_optimizer_preserves_javascript_numeric_edges() {
    let mut compiler = OptimizedCompiler;
    let negative_zero = compiler
        .compile(&[
            OptimizedInput::constant_f64(-0.0),
            OptimizedInput::constant_f64(1.0),
            OptimizedInput::binary(NumericBinaryOp::Mul, 0, 1),
            OptimizedInput::ret(2),
        ])
        .expect("numeric input is supported");
    assert!(negative_zero.constant(2).unwrap().is_negative_zero());

    let overflow = compiler
        .compile(&[
            OptimizedInput::constant_i32(i32::MAX),
            OptimizedInput::constant_i32(1),
            OptimizedInput::binary(NumericBinaryOp::Add, 0, 1),
            OptimizedInput::ret(2),
        ])
        .expect("overflow widens exactly");
    assert_eq!(
        overflow.constant(2).unwrap().as_f64(),
        Some(2_147_483_648.0)
    );

    let nan = compiler
        .compile(&[
            OptimizedInput::constant_f64(0.0),
            OptimizedInput::constant_f64(0.0),
            OptimizedInput::binary(NumericBinaryOp::Div, 0, 1),
            OptimizedInput::ret(2),
        ])
        .unwrap();
    assert!(nan.constant(2).unwrap().as_f64().unwrap().is_nan());
}

#[test]
fn integer_zero_times_negative_one_folds_to_negative_zero() {
    let mut compiler = OptimizedCompiler;
    let function = compiler
        .compile(&[
            OptimizedInput::constant_i32(0),
            OptimizedInput::constant_i32(-1),
            OptimizedInput::binary(NumericBinaryOp::Mul, 0, 1),
            OptimizedInput::ret(2),
        ])
        .unwrap();

    assert!(function.constant(2).unwrap().is_negative_zero());
}

#[test]
fn local_cse_and_dce_are_effect_free_and_bounded() {
    let mut compiler = OptimizedCompiler;
    let function = compiler
        .compile(&[
            OptimizedInput::constant_i32(4),
            OptimizedInput::constant_i32(5),
            OptimizedInput::binary(NumericBinaryOp::Add, 0, 1),
            OptimizedInput::binary(NumericBinaryOp::Add, 0, 1),
            OptimizedInput::binary(NumericBinaryOp::Mul, 2, 3),
            OptimizedInput::constant_i32(99),
            OptimizedInput::ret(4),
        ])
        .unwrap();
    assert_eq!(function.cse_eliminated(), 1);
    assert_eq!(function.dead_nodes_eliminated(), 2);
    assert_eq!(function.constant(4).unwrap().as_f64(), Some(81.0));
}

#[test]
fn cse_rewrites_uses_to_the_canonical_ssa_value() {
    let mut compiler = OptimizedCompiler;
    let function = compiler
        .compile(&[
            OptimizedInput::constant_i32(4),
            OptimizedInput::constant_i32(5),
            OptimizedInput::binary(NumericBinaryOp::Add, 0, 1),
            OptimizedInput::binary(NumericBinaryOp::Add, 0, 1),
            OptimizedInput::binary(NumericBinaryOp::Mul, 3, 3),
            OptimizedInput::ret(4),
        ])
        .unwrap();

    assert_eq!(function.cse_eliminated(), 1);
    assert_eq!(function.representative(3), Some(2));
    assert_eq!(function.operands(4), Some((2, 2)));
    assert_eq!(function.constant(4).unwrap().as_f64(), Some(81.0));
}

#[test]
fn mixed_numeric_folding_preserves_negative_zero_nan_and_overflow() {
    let mut compiler = OptimizedCompiler;
    let function = compiler
        .compile(&[
            OptimizedInput::constant_i32(i32::MIN),
            OptimizedInput::constant_i32(-1),
            OptimizedInput::binary(NumericBinaryOp::Mul, 0, 1),
            OptimizedInput::constant_f64(-0.0),
            OptimizedInput::constant_i32(1),
            OptimizedInput::binary(NumericBinaryOp::Mul, 3, 4),
            OptimizedInput::constant_f64(f64::NAN),
            OptimizedInput::binary(NumericBinaryOp::Add, 6, 4),
            OptimizedInput::ret(7),
        ])
        .unwrap();

    assert_eq!(
        function.constant(2).unwrap().as_f64(),
        Some(2_147_483_648.0)
    );
    assert!(function.constant(5).unwrap().is_negative_zero());
    assert!(function.constant(7).unwrap().as_f64().unwrap().is_nan());
}

#[test]
fn dependency_invalidation_is_transitive_and_generation_exact() {
    let a = DependencyKey::function(FunctionKey::new(1, 1));
    let b = DependencyKey::function(FunctionKey::new(2, 1));
    let c = DependencyKey::function(FunctionKey::new(3, 1));
    let mut graph = DependencyGraph::default();
    graph.install(c, 1, []).unwrap();
    graph.install(b, 1, [c]).unwrap();
    graph.install(a, 1, [b]).unwrap();

    let invalidated = graph.invalidate(c);
    assert_eq!(invalidated.len(), 3);
    assert!(!graph.validate_install(a, 1, &[(b, 1)]));

    let b2 = DependencyKey::function(FunctionKey::new(2, 2));
    graph.install(b2, 1, []).unwrap();
    assert!(graph.validate_install(b2, 1, &[]));

    assert!(graph
        .invalidate(DependencyKey::function(FunctionKey::new(99, 1)))
        .is_empty());
}

#[test]
fn tier2_plan_is_exact_for_numeric_locals_and_rejects_unsupported_semantics() {
    let numeric = SnapshotFixture::compile(
        "(function(n,zero){let s=zero; for(let i=zero;i<n;i++) s=s+i; return s})",
    );
    let numeric = numeric.snapshot().verify(VerifyLimits::default()).unwrap();
    let metadata = Tier2Compiler::plan(&numeric, 23).expect("local numeric loop is narrow Tier2");
    assert_eq!(metadata.feedback_epoch(), 23);
    assert!(metadata.boxes_elided() > 0);
    assert!(!metadata.deopt_sites().is_empty());
    assert!(metadata
        .deopt_sites()
        .iter()
        .all(|(shape, map)| map.validate(*shape).is_ok()));

    let property = SnapshotFixture::compile("(function(){return globalThis.answer})");
    let property = property.snapshot().verify(VerifyLimits::default()).unwrap();
    Tier2Compiler::plan(&property, 24).expect("global lookups and property loads are exact");

    let arguments_object = SnapshotFixture::compile("(function(){return arguments.length})");
    let arguments_object = arguments_object
        .snapshot()
        .verify(VerifyLimits::default())
        .unwrap();
    assert!(Tier2Compiler::plan(&arguments_object, 25).is_err());
}

#[test]
fn production_optimized_ir_is_independent_ssa_with_loop_guards() {
    let fixture = SnapshotFixture::compile(
        "(function(n,zero){let dead=40+2;let s=zero;for(let i=zero;i<n;i++)s=s+i;return s})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 31).expect("numeric loop is optimized directly");

    assert!(ir.blocks().iter().any(|block| block.is_loop_header()));
    assert!(ir
        .nodes()
        .iter()
        .any(|node| matches!(node.kind(), OptimizedNodeKind::GuardNumeric { .. })));
    assert!(ir
        .nodes()
        .iter()
        .any(|node| node.representation() == ValueRepresentation::Float64));
    assert!(ir
        .nodes()
        .iter()
        .all(|node| node.effect() != OptimizedEffect::Reentrant));
    assert!(
        ir.guard_maps().len() >= 2,
        "entry and mid-loop maps are required"
    );
    assert!(ir
        .guard_maps()
        .iter()
        .all(|site| site.map().guard() == site.guard()));
    assert!(ir
        .guard_maps()
        .iter()
        .all(|site| site.map().validate(site.shape()).is_ok()));
}

#[test]
fn iterative_fibonacci_multi_local_loop_translates_to_optimized_ir() {
    let fixture = SnapshotFixture::compile(
        "(function(_iterations,seed){let a=seed;let b=1;for(let i=seed;i<40;i++){const next=a+b;a=b;b=next;}return a})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    verified.tier1_eligibility().unwrap_or_else(|error| {
        panic!(
            "iterative Fibonacci is not advertised for Tier1: {error:?}; opcodes={:?}",
            verified
                .instructions()
                .iter()
                .map(|instruction| (instruction.pc(), instruction.opcode().name()))
                .collect::<Vec<_>>()
        )
    });
    let ir = OptimizedIr::translate(&verified, 32)
        .unwrap_or_else(|error| panic!("iterative Fibonacci did not translate: {error:?}"));
    assert!(ir.blocks().iter().any(|block| block.is_loop_header()));
    rquickjs_jit::test_support::compile_implemented_fixture(
        &rquickjs_jit::compiler::baseline::BaselineCompiler::host(),
        &verified,
    )
    .unwrap_or_else(|error| panic!("iterative Fibonacci did not lower in Tier1: {error:?}"));
    Tier2Compiler::host(32)
        .lower_for_test(&verified, 32)
        .unwrap_or_else(|error| panic!("iterative Fibonacci did not lower: {error:?}"));

    let batched = SnapshotFixture::compile(
        "(function(iterations,seed){let result=seed;for(let batch=seed;batch<iterations;batch++){let a=seed;let b=1;for(let i=seed;i<40;i++){const next=a+b;a=b;b=next;}result=a;}return result})",
    );
    let batched = batched.snapshot().verify(VerifyLimits::default()).unwrap();
    batched.tier1_eligibility().unwrap_or_else(|error| {
        panic!(
            "batched iterative Fibonacci is not Tier1: {error:?}; opcodes={:?}",
            batched
                .instructions()
                .iter()
                .map(|instruction| (instruction.pc(), instruction.opcode().name()))
                .collect::<Vec<_>>()
        )
    });
    let key = FunctionKey::new(
        batched.snapshot().function_id(),
        batched.snapshot().generation(),
    );
    let return_pc = batched
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .unwrap()
        .pc();
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Int32]);
        for instruction in batched
            .instructions()
            .iter()
            .filter(|instruction| instruction.opcode().name() == "add")
        {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let clif = Tier2Compiler::host(33)
        .lower_with_feedback_for_test(&batched, key, &feedback.snapshot(33))
        .unwrap_or_else(|error| panic!("batched iterative Fibonacci did not lower: {error:?}"));
    assert!(
        clif.lines()
            .any(|line| line.starts_with("block") && line.matches(": i32").count() >= 4),
        "the a/b/i/batch loop phis must remain raw i32: {clif}"
    );
    assert!(clif.contains("sadd_overflow"), "{clif}");
    assert!(!clif.contains("fadd"), "{clif}");
    assert!(!clif.contains("fcvt_from_sint"), "{clif}");
}

#[test]
fn tier2_rejects_captured_loop_headers_with_live_operand_stack() {
    use rquickjs_core::qjs;
    let bytecode = vec![
        qjs::QJS_JIT_OP_PUSH_TRUE,
        qjs::QJS_JIT_OP_DUP,
        qjs::QJS_JIT_OP_IF_TRUE8,
        (-2i8) as u8,
        qjs::QJS_JIT_OP_RETURN,
    ];
    let verified = CompileSnapshot::from_untrusted_bytecode(bytecode, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .expect("captured loop with a live stack value is well formed");
    assert!(OptimizedIr::translate(&verified, 1).is_err());
}

#[test]
fn independent_optimized_machine_lowers_numeric_loop() {
    let fixture = SnapshotFixture::compile(
        "(function(n,zero){let s=zero;for(let i=zero;i<n;i++)s=s+i;return s})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let clif = Tier2Compiler::host(33)
        .lower_for_test(&verified, 33)
        .unwrap();
    assert!(clif.contains("fadd"));
    assert!(clif.contains("brif"));
    let fixture = SnapshotFixture::compile(
        "(function(n,zero){let unused;let s=zero;for(let i=zero;i<n;i++)s=s+i;return s})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    Tier2Compiler::host(34)
        .lower_for_test(&verified, 34)
        .unwrap();
}

#[test]
fn stable_int32_add_feedback_selects_a_guarded_integer_only_hot_path() {
    let fixture = SnapshotFixture::compile("(function add(a,b){return a+b})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let add_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "add")
        .unwrap()
        .pc();
    let return_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .unwrap()
        .pc();
    let mut feedback = FeedbackTable::new(16, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Int32]);
        feedback.observe_binary(
            key,
            add_pc,
            ObservedType::Int32,
            ObservedType::Int32,
            ObservedType::Int32,
            Default::default(),
        );
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }

    let clif = Tier2Compiler::host(61)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(61))
        .expect("stable Int32 add feedback is specialized");

    assert!(clif.contains("sadd_overflow"), "{clif}");
    assert!(
        !clif.contains("fadd"),
        "the Int32 hot path must not compute a float add: {clif}"
    );
    assert!(
        !clif
            .lines()
            .any(|line| line.trim_start().starts_with("call ")),
        "the Int32 add hot path must not call a helper: {clif}"
    );
}

#[test]
fn stable_float64_feedback_selects_strict_direct_float_arithmetic() {
    let fixture =
        SnapshotFixture::compile("(function arithmetic(a,b){return (a+b)+(a-b)+(a*b)+(a/b)})");
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
    let mut feedback = FeedbackTable::new(32, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Float64, ObservedType::Float64]);
        for instruction in verified.instructions().iter().filter(|instruction| {
            matches!(instruction.opcode().name(), "add" | "sub" | "mul" | "div")
        }) {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Float64,
                ObservedType::Float64,
                ObservedType::Float64,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Float64);
    }

    let clif = Tier2Compiler::host(62)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(62))
        .expect("stable Float64 arithmetic feedback is specialized");

    for operation in ["fadd", "fsub", "fmul", "fdiv"] {
        assert!(clif.contains(operation), "missing {operation}: {clif}");
    }
    assert!(!clif.contains("fcvt_from_sint"), "{clif}");
    assert!(!clif.contains("sadd_overflow"), "{clif}");
    assert!(
        !clif
            .lines()
            .any(|line| line.trim_start().starts_with("call ")),
        "the Float64 hot path must not call a helper: {clif}"
    );
}

#[test]
fn stable_int32_loop_carries_unboxed_i32_ssa_through_the_header() {
    let fixture =
        SnapshotFixture::compile("(function sum(n,z){let s=z;for(let i=z;i<n;i++)s=s+i;return s})");
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Int32]);
        for instruction in verified.instructions().iter().filter(|instruction| {
            matches!(instruction.opcode().name(), "add" | "sub" | "mul" | "div")
        }) {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let opcodes = verified
        .instructions()
        .iter()
        .map(|instruction| instruction.opcode().name())
        .collect::<Vec<_>>();
    let clif = Tier2Compiler::host(63)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(63))
        .unwrap();

    assert!(
        clif.lines()
            .any(|line| { line.starts_with("block") && line.matches(": i32").count() >= 2 }),
        "opcodes={opcodes:?}\n{clif}"
    );
    assert!(clif.contains("sadd_overflow"), "{clif}");
    assert!(!clif.contains("fcvt_from_sint"), "{clif}");
    assert_eq!(
        clif.matches("ireduce.i32").count(),
        2,
        "only the two entry arguments may be unboxed: {clif}"
    );
    let expected_calls = if cfg!(rquickjs_memory_sanitizer) {
        4
    } else {
        1
    };
    assert_eq!(
        clif.matches("call_indirect").count(),
        expected_calls,
        "only the amortized interrupt poll helper is permitted: {clif}"
    );
}

#[test]
fn stable_packed_element_loop_lowers_to_native_tier2_loads() {
    let fixture = SnapshotFixture::compile(
        "(function sum(values){let s=0;for(let i=0;i<values.length;i++)s=(s+values[i])|0;return s})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    assert!(verified
        .instructions()
        .iter()
        .any(|instruction| instruction.opcode().name() == "get_array_el"));
    OptimizedIr::translate(&verified, 163)
        .unwrap_or_else(|error| panic!("element loop IR translation failed: {error:?}"));
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Object]);
        for instruction in verified.instructions().iter().filter(|instruction| {
            matches!(instruction.opcode().name(), "add" | "sub" | "mul" | "div")
        }) {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }

    let clif = Tier2Compiler::host(163)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(163))
        .unwrap_or_else(|error| panic!("element loop did not lower: {error:?}"));
    assert!(clif.contains("load.i16"), "class guard missing: {clif}");
    assert!(
        clif.contains("load.i32"),
        "bounds/element load missing: {clif}"
    );
    assert!(
        clif.contains("sadd_overflow"),
        "Int32 SSA add missing: {clif}"
    );
}

#[test]
fn profiled_packed_loads_reuse_guarded_metadata_within_the_hot_block() {
    let fixture = SnapshotFixture::compile(
        "(function sum(values,n){let s=0;for(let i=0;i<n;i++)s=(s+values[i]+values[i])|0;return s})",
    );
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Object, ObservedType::Int32]);
        for instruction in verified.instructions() {
            match instruction.opcode().name() {
                "get_array_el" => {
                    feedback.observe_array(
                        key,
                        instruction.pc(),
                        ArrayAccess::Load,
                        ArrayMode::Packed,
                        ArrayHazards::NONE,
                    );
                }
                "add" => {
                    feedback.observe_binary(
                        key,
                        instruction.pc(),
                        ObservedType::Int32,
                        ObservedType::Int32,
                        ObservedType::Int32,
                        Default::default(),
                    );
                }
                _ => {}
            }
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let clif = Tier2Compiler::host(165)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(165))
        .expect("profiled packed loop lowers");
    assert_eq!(
        clif.matches("load.i16").count(),
        1,
        "the second load must reuse the first guarded class/count/data tuple: {clif}"
    );
    assert!(
        clif.matches("icmp.i32 ult").count() >= 2,
        "each dynamic index still needs its own dense-count bounds guard: {clif}"
    );
}

#[test]
fn packed_counted_loop_hoists_metadata_and_deletes_the_proven_bounds_check() {
    let fixture = SnapshotFixture::compile(
        // Keep the observable length read in the loop condition.  The packed
        // metadata guard proves the ordinary Array length is the exact dense
        // count, so the guarded path should reuse that value instead of
        // materializing the frame for a generic lookup on every iteration.
        "(function sum(values){let s=0;for(let i=0;i<values.length;i++)s=(s+values[i])|0;return s})",
    );
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Object]);
        for instruction in verified.instructions() {
            match instruction.opcode().name() {
                "get_length" => {
                    feedback.observe_array(
                        key,
                        instruction.pc(),
                        ArrayAccess::Length,
                        ArrayMode::Packed,
                        ArrayHazards::NONE,
                    );
                }
                "get_array_el" => {
                    feedback.observe_array(
                        key,
                        instruction.pc(),
                        ArrayAccess::Load,
                        ArrayMode::Packed,
                        ArrayHazards::NONE,
                    );
                }
                "add" => {
                    feedback.observe_binary(
                        key,
                        instruction.pc(),
                        ObservedType::Int32,
                        ObservedType::Int32,
                        ObservedType::Int32,
                        Default::default(),
                    );
                }
                "lt" => {
                    feedback.observe_binary(
                        key,
                        instruction.pc(),
                        ObservedType::Int32,
                        ObservedType::Int32,
                        ObservedType::Bool,
                        Default::default(),
                    );
                }
                "or" => {
                    feedback.observe_binary(
                        key,
                        instruction.pc(),
                        ObservedType::Int32,
                        ObservedType::Int32,
                        ObservedType::Int32,
                        Default::default(),
                    );
                }
                _ => {}
            }
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let clif = Tier2Compiler::host(166)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(166))
        .expect("canonical packed traversal lowers");
    assert_eq!(
        clif.matches("load.i16").count(),
        2,
        "metadata must be checked in the preheader and only on the amortized poll slow path: {clif}"
    );
    assert_eq!(
        clif.matches("icmp.i32 ult").count(),
        0,
        "the range-proven loop load retained a per-iteration bounds check: {clif}"
    );
    assert!(
        clif.contains("load.i64") && clif.contains("icmp_imm sge"),
        "the packed value/hole guard must remain after bounds elimination: {clif}"
    );
}

#[test]
fn typed_counted_loop_uses_effect_free_length_guard_and_cached_storage() {
    let fixture = SnapshotFixture::compile(
        "(function sum(values){let s=0;for(let i=0;i<values.length;i++)s=(s+values[i])|0;return s})",
    );
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Object]);
        for instruction in verified.instructions() {
            match instruction.opcode().name() {
                "get_length" => {
                    feedback.observe_array(
                        key,
                        instruction.pc(),
                        ArrayAccess::Length,
                        ArrayMode::Int32,
                        ArrayHazards::NONE,
                    );
                }
                "get_array_el" => {
                    feedback.observe_array(
                        key,
                        instruction.pc(),
                        ArrayAccess::Load,
                        ArrayMode::Int32,
                        ArrayHazards::NONE,
                    );
                }
                "add" | "or" => {
                    feedback.observe_binary(
                        key,
                        instruction.pc(),
                        ObservedType::Int32,
                        ObservedType::Int32,
                        ObservedType::Int32,
                        Default::default(),
                    );
                }
                "lt" => {
                    feedback.observe_binary(
                        key,
                        instruction.pc(),
                        ObservedType::Int32,
                        ObservedType::Int32,
                        ObservedType::Bool,
                        Default::default(),
                    );
                }
                _ => {}
            }
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let clif = Tier2Compiler::host(167)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(167))
        .expect("canonical typed traversal lowers");
    assert_eq!(
        clif.matches("explicit_slot 24").count(),
        2,
        "typed metadata must be queried only in the preheader and cold poll path: {clif}"
    );
    assert_eq!(
        clif.matches("icmp.i32 ult").count(),
        0,
        "the exact intrinsic length/count proof must cover loop bounds: {clif}"
    );
    assert!(
        clif.contains("load.i32"),
        "cached Int32 load missing: {clif}"
    );
}

#[test]
fn typed_store_reuses_only_live_guarded_storage_for_the_following_load() {
    let fixture = SnapshotFixture::compile("(function(a,i,v){a[i]=v;return a[i]})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let mut feedback = FeedbackTable::new(64, 2);
    feedback.observe_call(
        key,
        &[
            ObservedType::Object,
            ObservedType::Int32,
            ObservedType::Int32,
        ],
    );
    for instruction in verified.instructions() {
        let access = match instruction.opcode().name() {
            "get_array_el" => ArrayAccess::Load,
            "put_array_el" => ArrayAccess::Store,
            _ => continue,
        };
        feedback.observe_array(
            key,
            instruction.pc(),
            access,
            ArrayMode::Int32,
            ArrayHazards::NONE,
        );
    }
    let clif = Tier2Compiler::host(165)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(165))
        .unwrap();
    assert_eq!(clif.matches("explicit_slot 24").count(), 1,
        "typed store must query attached fixed nonshared storage before publishing cached metadata: {clif}");
}

#[test]
fn stable_typed_element_loop_lowers_native_loads_and_stores() {
    let fixture = SnapshotFixture::compile(
        "(function convert(ints,floats){let sum=0.0;for(let i=0;i<ints.length;i++){floats[i]=ints[i]*0.25+0.5;sum=sum+floats[i];}return sum})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    assert!(verified
        .instructions()
        .iter()
        .any(|instruction| instruction.opcode().name() == "put_array_el"));
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Object, ObservedType::Object]);
        for instruction in verified.instructions().iter().filter(|instruction| {
            matches!(instruction.opcode().name(), "add" | "sub" | "mul" | "div")
        }) {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Float64,
                ObservedType::Float64,
                ObservedType::Float64,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Float64);
    }
    let clif = Tier2Compiler::host(164)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(164))
        .unwrap_or_else(|error| {
            panic!(
                "typed element loop did not lower: {error:?}; opcodes={:?}",
                verified
                    .instructions()
                    .iter()
                    .map(|instruction| instruction.opcode().name())
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        clif.contains("fcvt_from_sint.f64")
            && clif.contains("uextend.i64")
            && clif.contains("imul_imm"),
        "typed address/conversion path missing: {clif}"
    );
    assert!(
        !clif.contains("imul_imm.i32"),
        "element byte offsets must not wrap at 32 bits: {clif}"
    );
    assert!(clif.contains("load.f64"), "typed load missing: {clif}");
    assert!(clif.contains("load.i8"), "detached guard missing: {clif}");
    assert_eq!(
        clif.matches("icmp ult").count(),
        1,
        "the load immediately following a stable in-bounds typed store did not reuse its guarded count and data pointer: {clif}"
    );
}

#[test]
fn stable_int32_sub_mul_div_use_checked_native_operations() {
    let fixture =
        SnapshotFixture::compile("(function arithmetic(a,b){return ((a-b)*(a+b))/(b+1)})");
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Int32]);
        for instruction in verified.instructions().iter().filter(|instruction| {
            matches!(instruction.opcode().name(), "add" | "sub" | "mul" | "div")
        }) {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let clif = Tier2Compiler::host(64)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(64))
        .unwrap();

    for operation in ["ssub_overflow", "smul_overflow", "sdiv", "srem"] {
        assert!(clif.contains(operation), "missing {operation}: {clif}");
    }
    assert!(!clif.contains("fsub"), "{clif}");
    assert!(!clif.contains("fmul"), "{clif}");
    assert!(!clif.contains("fdiv"), "{clif}");
    assert!(
        clif.matches("brif").count() >= 7,
        "division guards missing: {clif}"
    );
    assert!(clif.contains("-2147483648"), "MIN/-1 guard missing: {clif}");
    assert!(clif.contains(", -1"), "MIN/-1 guard missing: {clif}");
    assert!(
        clif.contains("slt") && clif.contains(", 0"),
        "negative-zero guard missing: {clif}"
    );
}

#[test]
fn stable_monomorphic_call_lowers_with_visible_owner_provenance() {
    let fixture = SnapshotFixture::compile("(function invoke(f,a){let x=f(a);return x+0})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let caller = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let call_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name().starts_with("call"))
        .expect("call opcode")
        .pc();
    let callee = FunctionKey::new(caller.id + 100, 1);
    let mut feedback = FeedbackTable::new(32, 2);
    for _ in 0..32 {
        feedback.observe_call_signature(
            caller,
            call_pc,
            callee,
            &[ObservedType::Int32],
            ObservedType::Int32,
        );
    }

    let clif = Tier2Compiler::host(65)
        .lower_with_feedback_for_test(&verified, caller, &feedback.snapshot(65))
        .expect("owned call bridge");
    assert!(clif.contains("call_indirect"), "{clif}");
}

#[test]
fn monomorphic_call_emits_pointer_guard_and_unboxed_native_abi() {
    let fixture = SnapshotFixture::compile("(function invoke(f,a){let x=f(a);return x+0})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let caller = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let call_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name().starts_with("call"))
        .unwrap()
        .pc();
    let callee = FunctionKey::new(caller.id + 100, 9);
    let mut feedback = FeedbackTable::new(32, 2);
    for _ in 0..32 {
        feedback.observe_call_signature_with_identity(
            caller,
            call_pc,
            callee,
            0x1234_5678,
            0x2234_5678,
            &[ObservedType::Int32],
            ObservedType::Int32,
        );
    }
    let clif = Tier2Compiler::host(68)
        .lower_with_direct_target_for_test(
            &verified,
            caller,
            &feedback.snapshot(68),
            call_pc,
            0x7654_3210,
        )
        .expect("direct native caller");
    assert!(clif.contains("(i64, i32) -> i32"), "{clif}");
    assert!(
        clif.contains("0x1234_5678"),
        "callee identity guard absent: {clif}"
    );
    assert!(
        clif.contains("0x2234_5678"),
        "callee bytecode guard absent: {clif}"
    );
    assert!(
        clif.contains("0x7654_3210"),
        "direct entry address absent: {clif}"
    );
    assert!(clif.contains("call_indirect"), "{clif}");
    let block_label = |text: &str| {
        text.trim()
            .split(['(', ':', ',', ' '])
            .next()
            .unwrap()
            .to_owned()
    };
    let mut blocks = std::collections::BTreeMap::<String, Vec<&str>>::new();
    let mut current = None;
    for line in clif.lines().map(str::trim) {
        if line.starts_with("block") {
            let label = block_label(line);
            blocks.insert(label.clone(), Vec::new());
            current = Some(label);
        } else if let Some(label) = &current {
            blocks
                .get_mut(label)
                .unwrap()
                .push(line.split(';').next().unwrap().trim());
        }
    }
    let comparison = |constant: &str| {
        blocks
            .iter()
            .find_map(|(block, lines)| {
                lines.iter().find_map(|line| {
                    let (result, instruction) = line.split_once(" = ")?;
                    let (_, operands) = instruction.strip_prefix("icmp_imm")?.split_once(" eq ")?;
                    let (input, value) = operands.split_once(", ")?;
                    (value == constant).then_some((
                        block.clone(),
                        result.to_owned(),
                        input.to_owned(),
                    ))
                })
            })
            .unwrap_or_else(|| panic!("missing equality guard for {constant}: {clif}"))
    };
    let branch = |block: &str, condition: &str| {
        blocks[block]
            .iter()
            .find_map(|line| {
                let arguments = line
                    .strip_prefix("brif ")?
                    .splitn(3, ", ")
                    .collect::<Vec<_>>();
                (arguments.len() == 3 && arguments[0] == condition)
                    .then(|| (block_label(arguments[1]), block_label(arguments[2])))
            })
            .unwrap_or_else(|| {
                panic!("guard result {condition} must control brif in {block}: {clif}")
            })
    };
    let predecessors = |target: &str| {
        blocks
            .iter()
            .filter_map(|(block, lines)| {
                lines
                    .iter()
                    .any(|line| {
                        (line.starts_with("brif ") || line.starts_with("jump "))
                            && line
                                .split_whitespace()
                                .any(|token| block_label(token) == target)
                    })
                    .then_some(block.clone())
            })
            .collect::<Vec<_>>()
    };
    let (object_block, object_result, object_payload) = comparison("0x1234_5678");
    let (bytecode_block, bytecode_result, bytecode_payload) = comparison("0x2234_5678");
    let object_predecessors = predecessors(&object_block);
    assert_eq!(
        object_predecessors.len(),
        1,
        "object identity has an unguarded incoming edge: {clif}"
    );
    let tag_block = &object_predecessors[0];
    let tag_result = blocks[tag_block]
        .iter()
        .find_map(|line| {
            let (result, instruction) = line.split_once(" = ")?;
            let (_, operands) = instruction.strip_prefix("icmp_imm")?.split_once(" eq ")?;
            let (_, value) = operands.split_once(", ")?;
            (value == "-1").then_some(result)
        })
        .unwrap_or_else(|| panic!("object identity predecessor must test JS_TAG_OBJECT: {clif}"));
    let (tag_match, tag_miss) = branch(tag_block, tag_result);
    assert_eq!(
        tag_match, object_block,
        "tag success must reach object identity: {clif}"
    );
    let (object_match, object_miss) = branch(&object_block, &object_result);
    assert_eq!(
        object_match, bytecode_block,
        "object identity success must reach the payload load: {clif}"
    );
    assert_eq!(
        tag_miss, object_miss,
        "both guards must reject through the recovery edge: {clif}"
    );
    assert_eq!(
        predecessors(&bytecode_block),
        std::slice::from_ref(&object_block),
        "payload dereference has an unguarded incoming edge: {clif}"
    );
    assert_ne!(tag_block, &object_block);
    assert_ne!(object_block, bytecode_block);
    let bytecode_load = format!("{bytecode_payload} = load.i64 {object_payload}+48");
    let payload_load = format!("load.i64 {object_payload}+48");
    assert_eq!(
        blocks
            .values()
            .flatten()
            .filter(|line| line.ends_with(&payload_load))
            .count(),
        1,
        "the only payload dereference must be in the guarded bytecode block: {clif}"
    );
    assert!(
        blocks[&bytecode_block].contains(&bytecode_load.as_str()),
        "bytecode guard must compare the rooted object's +48 identity load: {clif}"
    );
    assert_eq!(
        branch(&bytecode_block, &bytecode_result).1,
        object_miss,
        "bytecode identity miss must recover: {clif}"
    );
    for block in [tag_block, &object_block] {
        assert!(
            !blocks[block]
                .iter()
                .any(|line| line.ends_with(&payload_load)),
            "payload was loaded before both guards succeeded: {clif}"
        );
    }
    assert!(
        clif.contains("brif") && clif.contains("return"),
        "guard/status mismatch must have exact deopt edge: {clif}"
    );
}

#[test]
fn direct_call_entry_uses_only_unboxed_int32_abi_and_checked_arithmetic() {
    let fixture = SnapshotFixture::compile("(function(a,b){return a+b})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let add_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "add")
        .unwrap()
        .pc();
    let return_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .unwrap()
        .pc();
    let mut feedback = FeedbackTable::new(32, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Int32]);
        feedback.observe_binary(
            key,
            add_pc,
            ObservedType::Int32,
            ObservedType::Int32,
            ObservedType::Int32,
            Default::default(),
        );
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let clif = Tier2Compiler::host(66)
        .lower_direct_call_with_feedback_for_test(&verified, key, &feedback.snapshot(66))
        .expect("typed direct-call entry");
    assert!(clif.contains("(i64, i32, i32) -> i32"), "{clif}");
    assert!(clif.contains("sadd_overflow"), "{clif}");
    assert!(
        !clif.contains("iconst.i64"),
        "tagged JSValue leaked into scalar body: {clif}"
    );
    assert!(
        !clif.contains("call_indirect"),
        "helper leaked into direct entry: {clif}"
    );
    assert_eq!(
        Tier2Compiler::host(66)
            .execute_direct_i32_for_test(&verified, key, &feedback.snapshot(66), &[20, 22])
            .unwrap(),
        (0, 42)
    );
    assert_eq!(
        Tier2Compiler::host(66)
            .execute_direct_i32_for_test(&verified, key, &feedback.snapshot(66), &[i32::MAX, 1])
            .unwrap()
            .0,
        1,
        "overflow must request exact CALL-site deopt"
    );
}

#[test]
fn direct_call_entry_uses_only_unboxed_float64_abi() {
    let fixture = SnapshotFixture::compile("(function(a,b){return a+b})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let add_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "add")
        .unwrap()
        .pc();
    let return_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .unwrap()
        .pc();
    let mut feedback = FeedbackTable::new(32, 2);
    for _ in 0..32 {
        feedback.observe_call(key, &[ObservedType::Float64, ObservedType::Float64]);
        feedback.observe_binary(
            key,
            add_pc,
            ObservedType::Float64,
            ObservedType::Float64,
            ObservedType::Float64,
            Default::default(),
        );
        feedback.observe_return(key, return_pc, ObservedType::Float64);
    }
    let clif = Tier2Compiler::host(67)
        .lower_direct_call_with_feedback_for_test(&verified, key, &feedback.snapshot(67))
        .expect("typed direct-call entry");
    assert!(clif.contains("(i64, f64, f64) -> i32"), "{clif}");
    assert!(clif.contains("fadd"), "{clif}");
    assert!(
        !clif.contains("call_indirect"),
        "helper leaked into direct entry: {clif}"
    );
}

#[test]
fn optimized_passes_rewrite_the_emitted_machine_plan() {
    let fixture = SnapshotFixture::compile("(function(a,b){a+b;return (a+b)+(a+b)})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 32).expect("pure numeric function");

    assert!(ir.metrics().cse_eliminated > 0, "{:#?}", ir.nodes());
    assert!(ir.machine_plan().iter().all(|node| !node.eliminated()));
    assert!(ir.machine_plan().len() < ir.nodes().len());

    let clif = Tier2Compiler::host(32)
        .lower_for_test(&verified, 32)
        .expect("rewritten plan lowers");
    assert_eq!(clif.matches("fadd").count(), 2, "{clif}");
}

#[test]
fn production_cse_keys_exact_ssa_operands_and_respects_frame_writes() {
    let distinct = SnapshotFixture::compile("(function(a,b,c,d,e,f){return (e-f)+(f-e)})");
    let distinct = distinct.snapshot().verify(VerifyLimits::default()).unwrap();
    let distinct_ir = OptimizedIr::translate(&distinct, 101).unwrap();
    assert_eq!(
        distinct_ir.metrics().cse_eliminated,
        0,
        "{:#?}",
        distinct_ir.nodes()
    );

    let mutation =
        SnapshotFixture::compile("(function(a,b){let x=a;let first=x-b;x=b;return first+(x-b)})");
    let mutation = mutation.snapshot().verify(VerifyLimits::default()).unwrap();
    let mutation_ir = OptimizedIr::translate(&mutation, 102).unwrap();
    assert_eq!(
        mutation_ir.metrics().cse_eliminated,
        0,
        "{:#?}",
        mutation_ir.nodes()
    );
}

#[test]
fn semantic_values_eliminate_repeated_nested_numeric_operations() {
    let fixture =
        SnapshotFixture::compile("(function(a,b,c,d){return ((a+b)*(c+d))+((a+b)*(c+d))})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let clif = Tier2Compiler::host(103)
        .lower_for_test(&verified, 103)
        .unwrap();
    assert_eq!(clif.matches("fmul").count(), 1, "{clif}");
    let ir = OptimizedIr::translate(&verified, 103).unwrap();
    let graph = ir.scalar_graph();
    assert!(graph.values().iter().any(|value| matches!(
        value,
        rquickjs_jit::ir::ScalarValue::Binary {
            op: rquickjs_jit::ir::ScalarBinaryOp::Mul, lhs, rhs, ..
        } if matches!(graph.values()[lhs.index()], rquickjs_jit::ir::ScalarValue::Binary { .. })
            && matches!(graph.values()[rhs.index()], rquickjs_jit::ir::ScalarValue::Binary { .. })
    )));
}

#[test]
fn semantic_values_preserve_operand_order_and_effect_boundaries() {
    for source in [
        "(function(a,b,c,d){return ((a-b)*(c-d))+((b-a)*(d-c))})",
        "(function(a,b,c,d){let first=(a+b)*(c+d);a=b;return first+(a+b)*(c+d)})",
        "(function(a,b,c,d,other){return ((a+b)*(c+d))+other()+((a+b)*(c+d))})",
    ] {
        let fixture = SnapshotFixture::compile(source);
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&verified, 104).unwrap();
        assert_eq!(
            ir.nodes()
                .iter()
                .filter(|node| matches!(
                    node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "mul"
                ))
                .count(),
            2,
            "{source}: {:#?}",
            ir.nodes()
        );
    }
}

#[test]
fn semantic_values_share_exact_integer_constants_in_expression_trees() {
    let fixture = SnapshotFixture::compile("(function(a,b){return ((a+1)*(b+2))+((a+1)*(b+2))})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let clif = Tier2Compiler::host(105)
        .lower_for_test(&verified, 105)
        .unwrap();
    assert_eq!(clif.matches("fmul").count(), 1);
}

#[cfg(all(
    target_os = "macos",
    target_endian = "little",
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
#[test]
fn semantic_values_execute_on_macos_and_deopt_before_object_coercion() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    for (expression, expected, must_deopt) in [
        ("String(target(2147483647,1,1,0))", "4294967296", false),
        ("String(Object.is(target(-0,-0,1,0), -0))", "true", false),
        ("String(Number.isNaN(target(NaN,1,1,0)))", "true", false),
        (
            "let hits=0; let a={valueOf(){hits++;return 1}}; target(a,2,3,4)+':'+hits",
            "42:2",
            true,
        ),
    ] {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .call_threshold(2)
                .force_optimized_for_test(true)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| {
                ctx.eval::<(), _>("function target(a,b,c,d){return ((a+b)*(c+d))+((a+b)*(c+d))}")
            })
            .unwrap();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
        while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
            assert_eq!(
                context
                    .with(|ctx| ctx.eval::<i32, _>("target(1,2,3,4)"))
                    .unwrap(),
                42
            );
            jit.poll();
        }
        let before = jit.metrics();
        assert!(before.tier2_entries >= 10, "{before:?}");
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<String, _>(expression))
                .unwrap(),
            expected,
            "{expression}"
        );
        let after = jit.metrics();
        assert!(
            after.tier2_entries > before.tier2_entries,
            "{expression}: {after:?}"
        );
        if must_deopt {
            assert!(after.deopts > before.deopts, "{expression}: {after:?}");
        }
    }
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_tier2_executes_packed_and_typed_element_loops() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

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
        .with(|ctx| {
            ctx.eval::<(), _>(
                r#"
                function elementSum(values) {
                  let sum = 0;
                  for (let i = 0; i < values.length; i++) sum = (sum + values[i]) | 0;
                  return sum;
                }
                function reassignedElement(x, y) {
                  let values = x;
                  let originalLength = values.length;
                  values = y;
                  return (originalLength + values[0]) | 0;
                }
                function branchedElement(x, y, select) {
                  let values = x;
                  let originalLength = values.length;
                  if (select) values = y;
                  return (originalLength + values[0]) | 0;
                }
                function reboundPackedLoads(x, y) {
                  let values = x;
                  let first = values[0];
                  values = y;
                  return (first + values[0]) | 0;
                }
                function reboundMixedLoads(x, y) {
                  let values = x;
                  let first = values[0];
                  values = y;
                  return (first + values[0]) | 0;
                }
                globalThis.packed = [1,2,3,4,5,6,7,8];
                globalThis.otherPacked = [99,98];
                globalThis.typed = new Int32Array(packed);
                globalThis.floatTyped = new Float64Array([1.5, 2.5]);
                globalThis.proxied = new Proxy([1, 2], {});
                "#,
            )
        })
        .unwrap();
    for _ in 0..10_000 {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("elementSum(packed)"))
                .unwrap(),
            36
        );
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
    let before = jit.metrics();
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("elementSum(packed)"))
            .unwrap(),
        36
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("elementSum(typed)"))
            .unwrap(),
        36
    );
    jit.poll();
    let after = jit.metrics();
    assert!(after.tier2_entries >= before.tier2_entries + 2, "{after:?}");
    // The caller was compiled from monomorphic Packed feedback.  Passing an
    // Int32Array must fail the exact ArrayMode guard before reading its
    // storage, then resume the original element access once.
    assert_eq!(after.deopts, before.deopts + 1, "{after:?}");
    assert_eq!(
        after.native_fallbacks,
        before.native_fallbacks + 1,
        "{after:?}"
    );
    for select in 0..2 {
        for _ in 0..64 {
            assert_eq!(
                context
                    .with(|ctx| { ctx.eval::<i32, _>("reassignedElement(packed, otherPacked)") })
                    .unwrap(),
                107
            );
            assert_eq!(
                context
                    .with(|ctx| {
                        ctx.eval::<i32, _>(format!(
                            "branchedElement(packed, otherPacked, {select})"
                        ))
                    })
                    .unwrap(),
                if select == 0 { 9 } else { 107 }
            );
            assert_eq!(
                context
                    .with(|ctx| ctx.eval::<i32, _>("reboundPackedLoads(packed, otherPacked)"))
                    .unwrap(),
                100
            );
            assert_eq!(
                context
                    .with(|ctx| ctx.eval::<i32, _>("reboundMixedLoads(packed, typed)"))
                    .unwrap(),
                2
            );
            jit.poll();
        }
    }
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("elementSum(floatTyped)"))
            .unwrap(),
        3
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("elementSum(proxied)"))
            .unwrap(),
        3
    );
    jit.poll();
    assert!(
        jit.metrics().deopts >= after.deopts + 2,
        "{:?}",
        jit.metrics()
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_tier2_truthiness_preserves_negative_zero_and_nan_without_fallback() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let captured = SnapshotFixture::compile("(function truth(v){return v?2:1})");
    let verified = captured.snapshot().verify(VerifyLimits::default()).unwrap();
    Tier2Compiler::host(91)
        .lower_for_test(&verified, 91)
        .expect("truthiness fixture must reach the production optimizing lowerer");

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
    context
        .with(|ctx| ctx.eval::<(), _>("function truth(v){return v?2:1}"))
        .unwrap();
    for _ in 0..8 {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("truth(-0)")).unwrap(),
            1
        );
    }
    for _ in 0..10_000 {
        jit.poll();
        if jit.metrics().installed > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().installed > 0, "{:?}", jit.metrics());
    for _ in 0..8 {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>("truth(-0)")).unwrap(),
            1
        );
    }
    for _ in 0..10_000 {
        jit.poll();
        if jit.metrics().installed >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().installed >= 2, "{:?}", jit.metrics());
    let before = jit.metrics();
    assert_eq!(
        context.with(|ctx| ctx.eval::<i32, _>("truth(-0)")).unwrap(),
        1
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("truth(NaN)"))
            .unwrap(),
        1
    );
    context.with(|ctx| {
        // Int32 and Bool use only the low 32 bits of QuickJS's value union.
        // Give the unused high bits a nonzero value to expose full-width tests.
        for (name, tag) in [
            ("paddedZero", rquickjs::qjs::JS_TAG_INT),
            ("paddedFalse", rquickjs::qjs::JS_TAG_BOOL),
        ] {
            let raw = rquickjs::qjs::JSValue {
                u: rquickjs::qjs::JSValueUnion {
                    float64: f64::from_bits(0x1234_5678_0000_0000),
                },
                tag: i64::from(tag),
            };
            let value = unsafe { rquickjs::Value::from_raw(ctx.clone(), raw) };
            ctx.globals().set(name, value).unwrap();
        }
    });
    for (expression, expected) in [
        ("truth(paddedZero)", 1),
        ("truth(paddedFalse)", 1),
        ("truth(0.0)", 1),
        ("truth(-NaN)", 1),
        ("truth(Infinity)", 2),
        ("truth(-Infinity)", 2),
        ("truth(Number.MIN_VALUE)", 2),
        ("truth(-Number.MIN_VALUE)", 2),
    ] {
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>(expression)).unwrap(),
            expected,
            "{expression}"
        );
    }
    jit.poll();
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert_eq!(after.native_fallbacks, before.native_fallbacks, "{after:?}");
    assert_eq!(after.native_retries, before.native_retries, "{after:?}");
    let before_object = jit.metrics();
    for _ in 0..1_024 {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("truth(globalThis.truthy ||= {})"))
                .unwrap(),
            2
        );
        jit.poll();
    }
    assert!(
        jit.metrics().deopts > before_object.deopts,
        "{:?}",
        jit.metrics()
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_feedback_installs_and_executes_the_int32_add_specialization() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(64)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| ctx.eval::<(), _>("function add(a,b){return a+b}"))
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while std::time::Instant::now() < deadline {
        assert_eq!(
            context.with(|ctx| {
                let add: rquickjs::Function<'_> = ctx.globals().get("add").unwrap();
                add.call::<_, i32>((20, 22)).unwrap()
            }),
            42
        );
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
    let artifact = jit.test_last_acquired_artifact_key().unwrap();
    assert_eq!(artifact.tier, Tier::Optimizing);
    assert_ne!(artifact.specialization_fingerprint, 0);

    let before = jit.metrics();
    assert_eq!(
        context.with(|ctx| {
            let add: rquickjs::Function<'_> = ctx.globals().get("add").unwrap();
            add.call::<_, i32>((7, 8)).unwrap()
        }),
        15
    );
    assert_eq!(
        context.with(|ctx| {
            let add: rquickjs::Function<'_> = ctx.globals().get("add").unwrap();
            add.call::<_, f64>((2_147_483_647_i32, 1_i32)).unwrap()
        }),
        2_147_483_648.0
    );
    assert_eq!(
        context.with(|ctx| {
            let add: rquickjs::Function<'_> = ctx.globals().get("add").unwrap();
            add.call::<_, String>(("a", "b")).unwrap()
        }),
        "ab"
    );
    jit.poll();
    let after = jit.metrics();
    assert!(after.tier2_entries >= before.tier2_entries + 3, "{after:?}");
    assert!(after.deopts >= before.deopts + 2, "{after:?}");
    assert!(
        after.deopt_materializations >= before.deopt_materializations + 2,
        "{after:?}"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_tier2_caller_executes_a_monomorphic_compiled_callee() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(64)
            .stress_gc(true)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function directAdd(a){return a+1}\n\
                 function invoke(f,a){let result=f(a);return result+0}\n\
                 function invokeTwice(f,a){let x=f(a);let y=f(x);return y+0}\n\
                 function recursiveCount(n){if(n<=0)return 0;return recursiveCount(n-1)+1}",
            )
        })
        .unwrap();

    for _ in 0..512 {
        assert_eq!(
            context.with(|ctx| {
                let add: rquickjs::Function<'_> = ctx.globals().get("directAdd").unwrap();
                let invoke: rquickjs::Function<'_> = ctx.globals().get("invoke").unwrap();
                invoke.call::<_, i32>((add, 41)).unwrap()
            }),
            42
        );
        assert_eq!(
            context.with(|ctx| {
                let add: rquickjs::Function<'_> = ctx.globals().get("directAdd").unwrap();
                let invoke: rquickjs::Function<'_> = ctx.globals().get("invokeTwice").unwrap();
                invoke.call::<_, i32>((add, 40)).unwrap()
            }),
            42
        );
        assert_eq!(
            context.with(|ctx| {
                let recursive: rquickjs::Function<'_> =
                    ctx.globals().get("recursiveCount").unwrap();
                recursive.call::<_, i32>((12,)).unwrap()
            }),
            12
        );
        jit.poll();
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let before = jit.metrics();
    assert!(before.installed >= 3, "{before:?}");
    assert_eq!(
        context.with(|ctx| {
            let add: rquickjs::Function<'_> = ctx.globals().get("directAdd").unwrap();
            let invoke: rquickjs::Function<'_> = ctx.globals().get("invoke").unwrap();
            invoke.call::<_, i32>((add, 99)).unwrap()
        }),
        100
    );
    for value in 0..2_048 {
        assert_eq!(
            context.with(|ctx| {
                let add: rquickjs::Function<'_> = ctx.globals().get("directAdd").unwrap();
                let invoke: rquickjs::Function<'_> = ctx.globals().get("invokeTwice").unwrap();
                invoke.call::<_, i32>((add, value)).unwrap()
            }),
            value + 2
        );
        assert_eq!(
            context.with(|ctx| {
                let recursive: rquickjs::Function<'_> =
                    ctx.globals().get("recursiveCount").unwrap();
                recursive.call::<_, i32>((8,)).unwrap()
            }),
            8
        );
    }
    jit.poll();
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert_eq!(after.native_fallbacks, before.native_fallbacks, "{after:?}");
    assert_eq!(after.native_retries, before.native_retries, "{after:?}");
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_tier2_waits_for_and_calls_a_direct_callee() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(16)
            .workers(1)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function increment(value,delta){return value+delta}\n\
                 function workload(iterations,seed,target){\n\
                   let value=seed;let delta=0;\n\
                   for(let i=0;i<iterations;i++){\n\
                     value=target(value,delta);\n\
                     delta++;if(delta<8){}else{delta=0;}\n\
                   }\n\
                   return value;\n\
                 }",
            )
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 75 } else { 15 });
    while std::time::Instant::now() < deadline {
        assert_eq!(
            context.with(|ctx| {
                let increment: Function<'_> = ctx.globals().get("increment").unwrap();
                let workload: Function<'_> = ctx.globals().get("workload").unwrap();
                workload.call::<_, i32>((128, 0, increment)).unwrap()
            }),
            448
        );
        jit.poll();
        if jit.metrics().installed >= 4 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }

    let before = jit.metrics();
    assert!(before.tier2_entries > 0, "{before:?}");
    for _ in 0..8 {
        assert_eq!(
            context.with(|ctx| {
                let increment: Function<'_> = ctx.globals().get("increment").unwrap();
                let workload: Function<'_> = ctx.globals().get("workload").unwrap();
                workload.call::<_, i32>((2_000, 0, increment)).unwrap()
            }),
            7_000
        );
    }
    let after = jit.metrics();
    assert!(
        after.tier2_entries.saturating_sub(before.tier2_entries) <= 16,
        "stable direct calls re-entered through QuickJS: before={before:?} after={after:?}"
    );
    assert_eq!(after.native_fallbacks, before.native_fallbacks, "{after:?}");
    assert_eq!(after.native_retries, before.native_retries, "{after:?}");
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_tier2_direct_call_checks_object_before_payload() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(64)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function directAdd(a){return a+1}\n\
                 function invoke(c,a){let fn=c?directAdd:5;return fn(a)}",
            )
        })
        .unwrap();

    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("invoke(true,41)"))
                .unwrap(),
            42
        );
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::yield_now();
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());

    assert_eq!(
        context
            .with(|ctx| {
                ctx.eval::<String, _>("try{invoke(false,1);'no error'}catch(e){String(e.name)}")
            })
            .unwrap(),
        "TypeError"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_unboxed_call_deopts_exactly_on_target_type_and_overflow_mismatch() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(64)
            .stress_gc(true)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function add1(a){return a+1}\n\
         function sub1(a){return a-1}\n\
         function addf(a,b){return a+b}\n\
         function invokeTarget(f,a){let x=f(a);return x+0}\n\
         function invokeType(f,a){let x=f(a);return x+0}\n\
         function invokeOverflow(f,a){let x=f(a);return x+0}\n\
         function invokef(f,a,b){let x=f(a,b);return x+0}",
            )
        })
        .unwrap();

    // Publish both typed callees before collecting/compiling their callers.
    for _ in 0..512 {
        context.with(|ctx| {
            let add1: rquickjs::Function<'_> = ctx.globals().get("add1").unwrap();
            assert_eq!(add1.call::<_, i32>((40,)).unwrap(), 41);
            let addf: rquickjs::Function<'_> = ctx.globals().get("addf").unwrap();
            assert_eq!(addf.call::<_, f64>((1.25, 2.5)).unwrap(), 3.75);
        });
        jit.poll();
    }
    for _ in 0..512 {
        context.with(|ctx| {
            let add1: rquickjs::Function<'_> = ctx.globals().get("add1").unwrap();
            for name in ["invokeTarget", "invokeType", "invokeOverflow"] {
                let invoke: rquickjs::Function<'_> = ctx.globals().get(name).unwrap();
                assert_eq!(invoke.call::<_, i32>((add1.clone(), 40)).unwrap(), 41);
            }
            let addf: rquickjs::Function<'_> = ctx.globals().get("addf").unwrap();
            let invokef: rquickjs::Function<'_> = ctx.globals().get("invokef").unwrap();
            assert_eq!(invokef.call::<_, f64>((addf, 1.25, 2.5)).unwrap(), 3.75);
        });
        jit.poll();
    }
    // Background compilation is much slower under coverage or sanitizer
    // instrumentation; let every queued caller version land before the
    // deoptimization probes run.
    let settled = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while std::time::Instant::now() < settled {
        let metrics = jit.metrics();
        if metrics.pending_worker_jobs == 0
            && metrics.pending_snapshot_bytes == 0
            && metrics.tier2_entries > 0
        {
            break;
        }
        context.with(|ctx| {
            let add1: rquickjs::Function<'_> = ctx.globals().get("add1").unwrap();
            for name in ["invokeTarget", "invokeType", "invokeOverflow"] {
                let invoke: rquickjs::Function<'_> = ctx.globals().get(name).unwrap();
                assert_eq!(invoke.call::<_, i32>((add1.clone(), 40)).unwrap(), 41);
            }
        });
        jit.poll();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let before = jit.metrics();
    assert!(before.tier2_entries > 0, "{before:?}");
    context.with(|ctx| {
        let invoke: rquickjs::Function<'_> = ctx.globals().get("invokeTarget").unwrap();
        let sub1: rquickjs::Function<'_> = ctx.globals().get("sub1").unwrap();
        assert_eq!(invoke.call::<_, i32>((sub1, 40)).unwrap(), 39);
        let invoke: rquickjs::Function<'_> = ctx.globals().get("invokeType").unwrap();
        let add1: rquickjs::Function<'_> = ctx.globals().get("add1").unwrap();
        assert_eq!(invoke.call::<_, f64>((add1, 1.5)).unwrap(), 2.5);
        let invoke: rquickjs::Function<'_> = ctx.globals().get("invokeOverflow").unwrap();
        let add1: rquickjs::Function<'_> = ctx.globals().get("add1").unwrap();
        assert_eq!(
            invoke.call::<_, f64>((add1, i32::MAX)).unwrap(),
            2_147_483_648.0
        );
    });
    jit.poll();
    let after = jit.metrics();
    assert!(after.deopts >= before.deopts + 2, "{before:?} -> {after:?}");
    assert!(
        after.deopt_materializations >= before.deopt_materializations + 2,
        "{before:?} -> {after:?}"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_worker_installs_and_enters_narrow_tier2_native_code() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let config = JitConfig::builder()
        .call_threshold(2)
        .loop_threshold(4)
        .stress_gc(true)
        .force_optimized_for_test(true)
        .build()
        .unwrap();
    let jit = Jit::attach(&runtime, config).unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| ctx.eval::<(), _>("function f(n,z){let s=z;for(let i=z;i<n;i++)s+=i;return s}"))
        .unwrap();
    let first = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("f").unwrap();
        function.call::<_, f64>((50_000, 0)).unwrap()
    });
    assert_eq!(first, 1_249_975_000.0);
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while std::time::Instant::now() < deadline {
        let last = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            function.call::<_, f64>((2000, 0)).unwrap()
        });
        assert_eq!(last, 1_999_000.0);
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
    assert!(jit.metrics().boxes_elided > 0, "{:?}", jit.metrics());
    assert_eq!(
        jit.test_last_acquired_artifact_key().unwrap().tier,
        rquickjs_jit::runtime::Tier::Optimizing
    );

    let before_strict_exits = jit.metrics();
    let overflowed = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("f").unwrap();
        function.call::<_, f64>((100_000, 0)).unwrap()
    });
    assert_eq!(overflowed, 4_999_950_000.0);
    jit.poll();
    let mixed = context.with(|ctx| {
        let function: Function<'_> = ctx.globals().get("f").unwrap();
        function.call::<_, f64>((2, 0.5)).unwrap()
    });
    assert_eq!(mixed, 2.5);
    jit.poll();
    let after_strict_exits = jit.metrics();
    assert!(
        after_strict_exits.deopts >= before_strict_exits.deopts + 2,
        "{after_strict_exits:?}"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn stable_int32_loop_waits_for_and_installs_a_bounded_raw_i32_version() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function genericLoop(n,z){let s=z;for(let i=z;i<n;i++)s=s+i;return s}",
            )
        })
        .unwrap();

    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while std::time::Instant::now() < deadline {
        let metrics = jit.metrics();
        if metrics.tier2_entries >= 10
            && metrics.pending_worker_jobs == 0
            && metrics.pending_snapshot_bytes == 0
        {
            break;
        }
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get("genericLoop").unwrap();
            function.call::<_, f64>((2_000, 0)).unwrap()
        });
        assert_eq!(result, 1_999_000.0);
        jit.poll();
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let settled = jit.metrics();
    assert!(settled.tier2_entries >= 10, "{settled:?}");
    assert_eq!(settled.pending_worker_jobs, 0, "{settled:?}");
    assert_eq!(settled.pending_snapshot_bytes, 0, "{settled:?}");
    assert_ne!(
        jit.test_last_acquired_artifact_key()
            .unwrap()
            .specialization_fingerprint,
        0,
        "production numeric Tier2 must carry its bounded feedback signature"
    );

    let before = jit.metrics();
    for _ in 0..10 {
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get("genericLoop").unwrap();
            function.call::<_, f64>((2_000, 0)).unwrap()
        });
        assert_eq!(result, 1_999_000.0);
        jit.poll();
    }
    let after = jit.metrics();
    assert_eq!(after.deopts, before.deopts, "{after:?}");
    assert_eq!(after.tier2_entries - before.tier2_entries, 10, "{after:?}");
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn iterative_fibonacci_enters_tier2_with_multi_local_loop_phis() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function fib(_iterations,seed){let a=seed;let b=1;for(let i=seed;i<40;i++){const next=a+b;a=b;b=next;}return a}",
            )
        })
        .unwrap();

    for _ in 0..10_000 {
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get("fib").unwrap();
            function.call::<_, i32>((2_000, 0)).unwrap()
        });
        assert_eq!(result, 102_334_155);
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());

    let before = jit.metrics();
    for _ in 0..10 {
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get("fib").unwrap();
            function.call::<_, i32>((2_000, 0)).unwrap()
        });
        assert_eq!(result, 102_334_155);
        jit.poll();
    }
    let after = jit.metrics();
    assert_eq!(after.deopts, before.deopts, "{after:?}");
    assert!(
        after.tier2_entries >= before.tier2_entries + 10,
        "{after:?}"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
#[cfg_attr(
    rquickjs_sanitizer,
    ignore = "absolute-time heuristic: instrumentation slows entry bookkeeping but not generated code"
)]
fn automatic_profitability_blacklist_unpublishes_harmful_baseline() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

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
        .with(|ctx| ctx.eval::<(), _>("function tiny(a,b){return a+b}"))
        .unwrap();

    for _ in 0..20_000 {
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get("tiny").unwrap();
            function.call::<_, i32>((20, 22)).unwrap()
        });
        assert_eq!(result, 42);
        jit.poll();
        if jit.metrics().interpreter_demotions > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let demoted = jit.metrics();
    assert_eq!(demoted.interpreter_demotions, 1, "{demoted:?}");
    assert!(demoted.profitability_rejected >= 5, "{demoted:?}");

    let native_before = demoted.native_entries;
    for _ in 0..20 {
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get("tiny").unwrap();
            function.call::<_, i32>((20, 22)).unwrap()
        });
        assert_eq!(result, 42);
        jit.poll();
    }
    assert_eq!(
        jit.metrics().native_entries,
        native_before,
        "{:?}",
        jit.metrics()
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn automatic_layout_kernel_reaches_tier2_without_host_polling() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

    // gpui-shell never calls `Jit::poll`: maintenance runs only when hot
    // probes find it due. Profitability retries must still follow their
    // short backoff instead of waiting a full periodic interval each.
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(&runtime, JitConfig::default()).unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function layoutKernel(batches,seed){\
                 let checksum=seed;\
                 for(let batch=0;batch<batches;batch+=1){\
                   let a=0;let b=1;\
                   for(let i=0;i<40;i+=1){const next=a+b;a=b;b=next;}\
                   checksum=b;\
                 }\
                 return checksum;\
                 }",
            )
        })
        .unwrap();
    let call = || {
        context.with(|ctx| {
            let function: Function = ctx.globals().get("layoutKernel").unwrap();
            assert_eq!(function.call::<_, i32>((100, 0)).unwrap(), 165_580_141);
        });
    };
    // Background compilation is far slower under sanitizer or coverage
    // instrumentation; bound the waits by time rather than by iterations.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 600 } else { 120 });
    while jit.metrics().native_entries == 0 {
        assert!(std::time::Instant::now() < deadline, "{:?}", jit.metrics());
        call();
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
    // Queueing happens on this thread, so the call count is deterministic;
    // the compile itself is timed by the (possibly contended) worker.
    let mut calls = 0;
    while jit.metrics().queued < 2 {
        assert!(std::time::Instant::now() < deadline, "{:?}", jit.metrics());
        call();
        calls += 1;
    }
    assert!(
        calls <= 60,
        "the Tier2 trial was queued {calls} calls after the Baseline without host polling: {:?}",
        jit.metrics()
    );
    while jit.metrics().tier2_entries == 0 {
        assert!(std::time::Instant::now() < deadline, "{:?}", jit.metrics());
        call();
        // Give the worker time to compile between calls, as frames do.
        std::thread::sleep(std::time::Duration::from_micros(500));
    }
    let metrics = jit.metrics();
    assert_eq!(metrics.interpreter_demotions, 0, "{metrics:?}");
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn automatic_gpui_layout_kernel_keeps_baseline_until_tier2_trial() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

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
            ctx.eval::<(), _>(
                "function layoutKernel(batches,seed){\
                 let checksum=seed;\
                 for(let batch=0;batch<batches;batch+=1){\
                   let a=0;let b=1;\
                   for(let i=0;i<40;i+=1){const next=a+b;a=b;b=next;}\
                   checksum=b;\
                 }\
                 return checksum;\
                 }\
                 function terminalHost(value){return JSON.stringify({value:value,kind:'panel'});}",
            )
        })
        .unwrap();

    // Each call runs for tens of microseconds, so native entry bookkeeping
    // cannot make its Baseline slower than the interpreter. Exhausting the
    // Tier2 profitability retries must fund the bounded trial while the
    // Baseline keeps running; returning to the interpreter would stall the
    // kernel for as long as the trial waits in the compile queue.
    let mut native_before: Option<u64> = None;
    // Background compilation is far slower under sanitizer or coverage
    // instrumentation; bound the wait by time rather than by iterations.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 600 } else { 120 });
    while std::time::Instant::now() < deadline {
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get("layoutKernel").unwrap();
            let result = function.call::<_, i32>((2_000, 0)).unwrap();
            let terminal: Function = ctx.globals().get("terminalHost").unwrap();
            assert_eq!(
                terminal.call::<_, String>((42,)).unwrap(),
                "{\"value\":42,\"kind\":\"panel\"}"
            );
            result
        });
        assert_eq!(result, 165_580_141);
        jit.poll();
        let metrics = jit.metrics();
        // `terminalHost` may still be demoted on its own call-only path, so
        // check the kernel through its native entries rather than the
        // runtime-wide demotion count.
        if let Some(before) = native_before {
            assert!(
                metrics.native_entries > before,
                "Baseline stopped entering native code before Tier2: {metrics:?}"
            );
        }
        if metrics.native_entries > 0 {
            native_before = Some(metrics.native_entries);
        }
        if metrics.tier2_entries > 0 {
            assert!(metrics.profitability_rejected >= 5, "{metrics:?}");
            assert_eq!(metrics.deopts, 0, "{metrics:?}");
            assert!(
                metrics.compile_failures > 0,
                "terminal host function must remain unsupported without blocking the profitable kernel: {metrics:?}"
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    panic!(
        "layout kernel never reached its Tier2 trial: {:?}",
        jit.metrics()
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn automatic_call_heavy_promotes_the_direct_edge_caller() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

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
            ctx.eval::<(), _>(
                "function increment(value,delta){return value+delta;}\
                 function workload(iterations,seed,target){\
                   let value=seed;let delta=0;\
                   for(let i=0;i<iterations;i+=1){\
                     value=target(value,delta);delta+=1;\
                     if(delta<8){}else{delta=0;}\
                   }\
                   return value;\
                 }",
            )
        })
        .unwrap();

    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 75 } else { 15 });
    while std::time::Instant::now() < deadline {
        jit.poll();
        let before = jit.metrics();
        if before.pending_worker_jobs == 0 && before.pending_snapshot_bytes == 0 {
            let result = context.with(|ctx| {
                let workload: Function = ctx.globals().get("workload").unwrap();
                let increment: Function = ctx.globals().get("increment").unwrap();
                workload.call::<_, i32>((2_000, 0, increment)).unwrap()
            });
            assert_eq!(result, 7_000);
            jit.poll();
            let after = jit.metrics();
            // The leaf's five baseline profitability retries are paced by
            // the runtime clock, while the Tier1-rejection blacklist and the
            // caller's first Tier2 entry can both happen earlier; faster
            // native loops finish that race sooner. Wait for the leaf's
            // retries to settle so the exact count below checks the caller.
            if after.blacklisted > 0
                && after.profitability_rejected >= 5
                && after.pending_worker_jobs == 0
                && after.pending_snapshot_bytes == 0
                && after.native_entries - before.native_entries == 1
                && after.tier2_entries - before.tier2_entries == 1
            {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let warmed = jit.metrics();
    assert!(warmed.blacklisted > 0, "{warmed:?}");
    assert_eq!(
        warmed.profitability_rejected, 5,
        "the stable direct-edge caller repeated the leaf's misleading baseline profitability retries: {warmed:?}"
    );
    assert!(
        warmed.profitability_approved > 0,
        "stable call IC trials must be approved: {warmed:?}"
    );
    assert_eq!(
        warmed.unsupported_opcode_failures, 0,
        "stable call ICs must not waste unsupported compilations: {warmed:?}"
    );
    assert_eq!(
        warmed.compile_failures, warmed.tier1_rejections,
        "only explicit Tier1 admission rejection is expected: {warmed:?}"
    );

    for _ in 0..11 {
        let result = context.with(|ctx| {
            let workload: Function = ctx.globals().get("workload").unwrap();
            let increment: Function = ctx.globals().get("increment").unwrap();
            workload.call::<_, i32>((2_000, 0, increment)).unwrap()
        });
        assert_eq!(result, 7_000);
        jit.poll();
    }
    let after = jit.metrics();
    let entries = after.native_entries.saturating_sub(warmed.native_entries);
    assert_eq!(
        after.tier2_entries - warmed.tier2_entries,
        11,
        "each warmed invocation must enter the Tier2 caller: before={warmed:?}, after={after:?}"
    );
    assert_eq!(after.unsupported_opcode_failures, 0, "{after:?}");
    assert!(
        entries <= 32,
        "automatic left the direct-edge caller in the interpreter and crossed into the leaf {entries} times: before={warmed:?}, after={after:?}"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn stable_float64_loop_stays_native_without_a_side_path() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};

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
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function stableFloat(n,z){let s=z;for(let i=z;i<n;i++)s=s+i;return s}",
            )
        })
        .unwrap();
    for _ in 0..8 {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<f64, _>("stableFloat(100.5,0.5)"))
                .unwrap(),
            5000.5
        );
    }
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while std::time::Instant::now() < deadline {
        jit.poll();
        if jit.metrics().installed >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().installed >= 2, "{:?}", jit.metrics());
    let before = jit.metrics();
    for _ in 0..10 {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<f64, _>("stableFloat(100000.5,0.5)"))
                .unwrap(),
            5_000_000_000.5
        );
        jit.poll();
    }
    let after_large = jit.metrics();
    assert_eq!(after_large.deopts, before.deopts, "{after_large:?}");
    assert_eq!(
        after_large.stable_path_compile_requests, before.stable_path_compile_requests,
        "{after_large:?}"
    );
    assert_eq!(after_large.side_path_entries, before.side_path_entries);
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("stableFloat(0.5,0.5)"))
            .unwrap(),
        0.5
    );
    jit.poll();
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("stableFloat(100000.5,0.5)"))
            .unwrap(),
        5_000_000_000.5
    );
    jit.poll();
    let after = jit.metrics();
    assert_eq!(after.deopts, before.deopts, "{after:?}");
    assert_eq!(
        after.side_path_entries, before.side_path_entries,
        "{after:?}"
    );
}

#[test]
fn worker_snapshot_contains_stable_tokens_not_runtime_pointers() {
    let function = FunctionKey::new(9, 4);
    let mut table = FeedbackTable::new(8, 3);
    table.observe_type(function, 21, FeedbackKind::Value, ObservedType::Int32);
    let snapshot = table.snapshot(17);

    assert_eq!(snapshot.epoch(), 17);
    assert_eq!(snapshot.entries()[0].function(), function);
    assert_eq!(snapshot.entries()[0].pc(), 21);
    assert_eq!(snapshot.entries()[0].observations(), &[ObservedType::Int32]);
}

#[test]
fn stable_side_exit_reaches_recompile_threshold_and_unstable_exit_demotes_with_backoff() {
    let key = FunctionKey::new(88, 1);
    let mut coordinator = Coordinator::with_limits(4, 4, 4, 1 << 20);
    for _ in 0..9 {
        assert_eq!(
            coordinator.record_optimized_side_exit(key, 7),
            SideExitAction::Counted
        );
    }
    assert_eq!(
        coordinator.record_optimized_side_exit(key, 7),
        SideExitAction::StablePathThreshold
    );
    coordinator.advance_clock(50);
    let SideExitAction::Demote { retry_after } = coordinator.record_optimized_side_exit(key, 8)
    else {
        panic!("a second guard is unstable")
    };
    assert!(retry_after > 50);
    assert_eq!(coordinator.metrics().optimized_demotions, 1);
}

#[test]
fn stable_side_exit_demotes_when_side_path_cannot_stop_repeated_failures() {
    let key = FunctionKey::new(88, 1);
    let mut coordinator = Coordinator::with_limits(4, 4, 4, 1 << 20);
    for count in 1..20 {
        assert_eq!(
            coordinator.record_optimized_side_exit(key, 7),
            if count == 10 {
                SideExitAction::StablePathThreshold
            } else {
                SideExitAction::Counted
            }
        );
    }
    assert!(matches!(
        coordinator.record_optimized_side_exit(key, 7),
        SideExitAction::Demote { .. }
    ));
    assert_eq!(coordinator.metrics().optimized_demotions, 1);
}

#[test]
fn stable_side_path_request_owns_exact_guard_profile_without_unloading_target() {
    use rquickjs_jit::runtime::{GuardId, SidePathProfile};
    let fixture = SnapshotFixture::compile("(function(a,b){return a-b})");
    let snapshot = fixture.snapshot();
    let key = FunctionKey::new(snapshot.function_id(), snapshot.generation());
    let verified = snapshot.verify(VerifyLimits::default()).unwrap();
    let mut feedback = FeedbackTable::new(8, 2);
    feedback.observe_type(key, 7, FeedbackKind::Exit, ObservedType::Float64);
    let frozen = feedback.snapshot(44);
    let mut coordinator = Coordinator::with_limits(4, 4, 4, 1 << 20);

    // A side-path request is a specialization of an already installed target.
    // The test helper installs the baseline and optimizing generations without
    // involving a worker; queuing must leave the optimizing pin available.
    coordinator
        .queue(key, Tier::Baseline, verified.clone())
        .unwrap();
    let baseline = coordinator.begin_next().unwrap();
    coordinator.complete(CompileCompletion {
        key,
        requested_tier: Tier::Baseline,
        artifact_key: baseline.artifact_key(),
        attempt_id: baseline.attempt_id(),
        result: Ok(CompiledArtifact::empty(baseline.artifact_key())),
    });
    coordinator
        .queue_with_feedback(key, Tier::Optimizing, verified.clone(), frozen.clone())
        .unwrap();
    let optimizing = coordinator.begin_next().unwrap();
    coordinator.complete(CompileCompletion {
        key,
        requested_tier: Tier::Optimizing,
        artifact_key: optimizing.artifact_key(),
        attempt_id: optimizing.attempt_id(),
        result: Ok(CompiledArtifact::empty(optimizing.artifact_key())),
    });
    let before = coordinator.pin(key, Tier::Optimizing).unwrap();
    assert!(coordinator.record_benefit(key, Tier::Optimizing, 10));
    assert!(coordinator.record_benefit(key, Tier::Optimizing, 20));
    let profile = SidePathProfile::new(key, GuardId::new(7), 7, ObservedType::Float64, 44);
    coordinator
        .queue_side_path(key, verified, frozen, profile)
        .unwrap();
    assert!(coordinator.pin(key, Tier::Optimizing).is_some());
    assert_eq!(before.key().generation, key.generation);
    // A useful side path must remain queueable/installable while the old
    // artifact continues hitting its guard, even beyond the failure budget.
    for _ in 0..24 {
        assert!(!matches!(
            coordinator.record_optimized_side_exit_profile(key, 7, Some(ObservedType::Float64)),
            SideExitAction::Demote { .. }
        ));
    }
    let request = coordinator.begin_next().unwrap();
    assert_eq!(request.side_path_profile(), Some(profile));
    assert_ne!(request.artifact_key().specialization_fingerprint, 0);
    for _ in 0..24 {
        assert!(!matches!(
            coordinator.record_optimized_side_exit_profile(key, 7, Some(ObservedType::Float64)),
            SideExitAction::Demote { .. }
        ));
    }
    coordinator.complete(CompileCompletion {
        key,
        requested_tier: Tier::Optimizing,
        artifact_key: request.artifact_key(),
        attempt_id: request.attempt_id(),
        result: Ok(CompiledArtifact::empty(request.artifact_key())),
    });
    assert_eq!(coordinator.metrics().stale_results, 0);
    assert_eq!(
        coordinator.pin(key, Tier::Optimizing).unwrap().key(),
        request.artifact_key()
    );
    assert!(coordinator.record_benefit(key, Tier::Optimizing, 7));
    assert_eq!(
        before.artifact().benefit().score,
        30,
        "replacement must not credit the still-pinned old artifact"
    );
    assert_eq!(
        coordinator
            .pin(key, Tier::Optimizing)
            .unwrap()
            .artifact()
            .benefit()
            .score,
        7
    );

    assert_eq!(
        coordinator.record_optimized_side_exit_profile(key, 7, Some(ObservedType::Float64)),
        SideExitAction::Counted,
        "replacement artifact starts a fresh guard failure budget"
    );
}

#[test]
fn primary_optimized_artifact_is_keyed_by_the_bounded_feedback_signature() {
    let fixture = SnapshotFixture::compile("(function(a,b){return a+b})");
    let snapshot = fixture.snapshot();
    let key = FunctionKey::new(snapshot.function_id(), snapshot.generation());
    let verified = snapshot.verify(VerifyLimits::default()).unwrap();
    let add_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "add")
        .unwrap()
        .pc();
    let return_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .unwrap()
        .pc();
    let mut feedback = FeedbackTable::new(8, 2);
    feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Int32]);
    feedback.observe_binary(
        key,
        add_pc,
        ObservedType::Int32,
        ObservedType::Int32,
        ObservedType::Int32,
        Default::default(),
    );
    feedback.observe_return(key, return_pc, ObservedType::Int32);
    let frozen = feedback.snapshot(77);
    let expected = frozen.bounded_specialization(key).unwrap().fingerprint();
    let mut coordinator = Coordinator::with_limits(4, 4, 4, 1 << 20);
    coordinator
        .queue(key, Tier::Baseline, verified.clone())
        .unwrap();
    let baseline = coordinator.begin_next().unwrap();
    coordinator.complete(CompileCompletion {
        key,
        requested_tier: Tier::Baseline,
        artifact_key: baseline.artifact_key(),
        attempt_id: baseline.attempt_id(),
        result: Ok(CompiledArtifact::empty(baseline.artifact_key())),
    });

    coordinator
        .queue_with_feedback(key, Tier::Optimizing, verified, frozen)
        .unwrap();
    let request = coordinator.begin_next().unwrap();
    assert_eq!(request.artifact_key().specialization_fingerprint, expected);
}

#[test]
fn side_path_queue_rejects_stale_or_mismatched_profiles() {
    use rquickjs_jit::runtime::{GuardId, SidePathProfile};
    let fixture = SnapshotFixture::compile("(function(a,b){return a-b})");
    let snapshot = fixture.snapshot();
    let key = FunctionKey::new(snapshot.function_id(), snapshot.generation());
    let verified = snapshot.verify(VerifyLimits::default()).unwrap();
    let mut coordinator = Coordinator::with_limits(4, 4, 4, 1 << 20);
    coordinator
        .queue(key, Tier::Baseline, verified.clone())
        .unwrap();
    let baseline = coordinator.begin_next().unwrap();
    coordinator.complete(CompileCompletion {
        key,
        requested_tier: Tier::Baseline,
        artifact_key: baseline.artifact_key(),
        attempt_id: baseline.attempt_id(),
        result: Ok(CompiledArtifact::empty(baseline.artifact_key())),
    });
    let mut initial = FeedbackTable::new(8, 2);
    initial.observe_type(key, 0, FeedbackKind::Value, ObservedType::Int32);
    coordinator
        .queue_with_feedback(key, Tier::Optimizing, verified.clone(), initial.snapshot(3))
        .unwrap();
    let optimizing = coordinator.begin_next().unwrap();
    coordinator.complete(CompileCompletion {
        key,
        requested_tier: Tier::Optimizing,
        artifact_key: optimizing.artifact_key(),
        attempt_id: optimizing.attempt_id(),
        result: Ok(CompiledArtifact::empty(optimizing.artifact_key())),
    });
    let feedback = FeedbackSnapshot::empty(5);
    let stale = SidePathProfile::new(
        FunctionKey::new(key.id, key.generation + 1),
        GuardId::new(0),
        0,
        ObservedType::String,
        5,
    );
    assert!(coordinator
        .queue_side_path(key, verified, feedback, stale)
        .is_err());
}

#[test]
fn guard_specific_float_side_path_changes_machine_guard_and_preserves_profile() {
    use rquickjs_jit::runtime::{GuardId, SidePathProfile};
    let fixture =
        SnapshotFixture::compile("(function(n,z){let s=z;for(let i=z;i<n;i++)s=s+i;return s})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let ir = OptimizedIr::translate(&verified, 55).unwrap();
    // Frame stores carry their own guard sites; the side path targets the
    // loop-header representation guard specifically.
    let loop_guard = ir
        .guard_maps()
        .iter()
        .find(|site| {
            site.guard() != 0
                && verified
                    .control_flow_graph()
                    .is_loop_header(site.map().resume_pc())
        })
        .unwrap();
    let profile = SidePathProfile::new(
        key,
        GuardId::new(loop_guard.guard()),
        loop_guard.map().resume_pc(),
        ObservedType::Float64,
        55,
    );
    let compiler = Tier2Compiler::host(55);
    let generic = compiler.lower_for_test(&verified, 55).unwrap();
    let specialized = compiler
        .lower_side_path_for_test(&verified, 55, profile)
        .unwrap();
    assert_ne!(
        generic, specialized,
        "side path must alter the selected guard block"
    );
    // Proven checks fold away in both versions, so count the side path's
    // own evidence: the guard block that records a side-path hit in the
    // frame flags before continuing on the alternate representation.
    let side_path_hits = |clif: &str| {
        clif.lines()
            .filter(|line| {
                line.contains("bor_imm")
                    && line.trim_end().ends_with(&format!(
                        ", {}",
                        rquickjs_core::qjs::JS_JIT_FRAME_SIDE_PATH_HIT
                    ))
            })
            .count()
    };
    assert!(
        side_path_hits(&specialized) > side_path_hits(&generic),
        "{specialized}"
    );
}

// ---------------------------------------------------------------------------
// M2 core opcodes: CLIF evidence for the native lowering.
// ---------------------------------------------------------------------------

fn tier2_clif(source: &str) -> String {
    let fixture = SnapshotFixture::compile(source);
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    Tier2Compiler::host(71)
        .lower_for_test(&verified, 71)
        .unwrap_or_else(|error| panic!("{source} did not lower: {error:?}"))
}

fn raw_clif(bytecode: Vec<u8>, arg_count: u16, local_count: u16) -> String {
    let verified = rquickjs_jit::test_support::verified_bytecode(bytecode, arg_count, local_count);
    Tier2Compiler::host(72)
        .lower_for_test(&verified, 72)
        .unwrap_or_else(|error| panic!("raw bytecode did not lower: {error:?}"))
}

#[test]
fn bitops_loop_lowers_to_native_shift_and_xor_instructions() {
    let clif = tier2_clif("(function(n,v){for(let i=0;i<n;i++)v=((v<<5)^(v>>>3)^i)|0;return v})");
    for operation in ["ishl", "ushr", "bxor", "bor"] {
        assert!(clif.contains(operation), "missing {operation}: {clif}");
    }
    assert!(
        clif.lines()
            .any(|line| line.contains("band_imm") && line.trim_end().ends_with(", 31")),
        "shift counts must be masked to five bits: {clif}"
    );
    // Immediate-only operands prove the fast path and its exits call no
    // helper at all: without a loop there is no poll, and every exit spills
    // primitives directly.
    let straight = raw_clif(
        vec![
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            1,
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            33,
            rquickjs_core::qjs::QJS_JIT_OP_SHL,
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            3,
            rquickjs_core::qjs::QJS_JIT_OP_SHR,
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            5,
            rquickjs_core::qjs::QJS_JIT_OP_XOR,
            rquickjs_core::qjs::QJS_JIT_OP_RETURN,
        ],
        0,
        0,
    );
    for operation in ["ishl", "ushr", "bxor"] {
        assert!(
            straight.contains(operation),
            "missing {operation}: {straight}"
        );
    }
    // A constant count with non-zero low five bits keeps `>>>` below 2^31,
    // so the result is an Int32 without a Float64 alternative.
    assert!(
        !straight.contains("fcvt_from_uint"),
        "constant unsigned shifts are always Int32: {straight}"
    );
    assert!(
        !straight.contains("call_indirect"),
        "bit operations must not call a helper: {straight}"
    );
    // A count whose low five bits are zero shifts by nothing: `-1 >>> 32`
    // is 4294967295 and must keep its Float64 representation.
    let unmasked = raw_clif(
        vec![
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            255,
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            32,
            rquickjs_core::qjs::QJS_JIT_OP_SHR,
            rquickjs_core::qjs::QJS_JIT_OP_RETURN,
        ],
        0,
        0,
    );
    assert!(
        unmasked.contains("fcvt_from_uint"),
        "a zero effective count may produce a Float64: {unmasked}"
    );
}

#[test]
fn modulo_loop_lowers_to_a_guarded_srem() {
    let clif = tier2_clif("(function(n){let s=0;for(let i=0;i<n;i++)s=s+(i%7);return s})");
    assert!(clif.contains("srem"), "{clif}");
    let straight = raw_clif(
        vec![
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            7,
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            3,
            rquickjs_core::qjs::QJS_JIT_OP_MOD,
            rquickjs_core::qjs::QJS_JIT_OP_RETURN,
        ],
        0,
        0,
    );
    assert!(straight.contains("srem"), "{straight}");
    assert!(
        !straight.contains("call_indirect"),
        "modulo must not call a helper: {straight}"
    );
}

#[test]
fn comparisons_lower_to_native_compares_without_helper_calls() {
    let lte = tier2_clif("(function(n){return n<=0})");
    assert!(lte.contains("fcmp"), "{lte}");
    let lte_immediate = raw_clif(
        vec![
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            1,
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_0,
            rquickjs_core::qjs::QJS_JIT_OP_LTE,
            rquickjs_core::qjs::QJS_JIT_OP_RETURN,
        ],
        0,
        0,
    );
    assert!(lte_immediate.contains("icmp"), "{lte_immediate}");
    assert!(!lte_immediate.contains("fcmp"), "{lte_immediate}");
    assert!(
        !lte_immediate.contains("call_indirect"),
        "`<=` must not call CompareSlow: {lte_immediate}"
    );
    let strict = tier2_clif("(function(x){return x===0})");
    assert!(strict.contains("fcmp"), "{strict}");
    let straight = raw_clif(
        vec![
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            1,
            rquickjs_core::qjs::QJS_JIT_OP_PUSH_I8,
            1,
            rquickjs_core::qjs::QJS_JIT_OP_STRICT_EQ,
            rquickjs_core::qjs::QJS_JIT_OP_RETURN,
        ],
        0,
        0,
    );
    assert!(straight.contains("fcmp"), "{straight}");
    assert!(
        !straight.contains("call_indirect"),
        "`===` must not call CompareSlow: {straight}"
    );
}

#[test]
fn tail_call_lowers_as_a_guarded_call_followed_by_the_done_exit() {
    let fixture = SnapshotFixture::compile("(function(f,x){return f(x)})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let tail = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "tail_call")
        .expect("`return f(x)` compiles to tail_call");
    let ir = OptimizedIr::translate(&verified, 73).unwrap();
    let expanded = ir
        .nodes()
        .iter()
        .filter(|node| node.pc() == tail.pc())
        .map(|node| match node.kind() {
            OptimizedNodeKind::Bytecode { opcode } => (
                opcode.to_string(),
                node.pops(),
                node.pushes(),
                node.deopt_guard(),
            ),
            other => panic!("unexpected node at the tail call: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(expanded.len(), 2, "{expanded:?}");
    assert_eq!(expanded[0].0, "call");
    assert_eq!((expanded[0].1, expanded[0].2), (2, 1));
    let guard = expanded[0]
        .3
        .expect("the synthesized call keeps a deopt guard");
    assert_eq!(expanded[1], ("return".to_string(), 1, 0, None));
    let site = ir
        .guard_maps()
        .iter()
        .find(|site| site.guard() == guard)
        .unwrap();
    assert_eq!(site.map().resume_pc(), tail.pc());
    assert_eq!(site.shape().stack(), 2);

    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let mut feedback = FeedbackTable::new(16, 2);
    for _ in 0..8 {
        feedback.observe_call_signature(
            key,
            tail.pc(),
            FunctionKey::new(99, 1),
            &[ObservedType::Int32],
            ObservedType::Int32,
        );
    }
    let clif = Tier2Compiler::host(73)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(73))
        .expect("tail call lowers through the audited call helper");
    assert!(clif.contains("call_indirect"), "{clif}");
    // Native callees never record call-site feedback; the tail call then
    // takes the generic CALL bridge and returns its owned result.
    let generic = Tier2Compiler::host(73)
        .lower_for_test(&verified, 73)
        .expect("a tail call without call-site feedback uses the generic bridge");
    assert!(generic.contains("call_indirect"), "{generic}");
}

/// Exact semantics of the M2 core opcodes, observed by executing the
/// published Tier 2 machine code on a synthetic frame. This bypasses the
/// Tier 1 policy entirely and checks results, exit kinds and resume pcs.
#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod m2_core_opcodes {
    use rquickjs_core::qjs;
    use rquickjs_jit::bytecode::opcode;
    use rquickjs_jit::compiler::optimized::Tier2Compiler;
    use rquickjs_jit::test_support::{verified_bytecode, JSValueRepr, SyntheticFrame};

    struct Run {
        kind: u32,
        map: u32,
        resume: Option<usize>,
        result: JSValueRepr,
    }

    fn run(bytecode: &[u8], arguments: &[JSValueRepr], locals: &[JSValueRepr]) -> Run {
        let verified = verified_bytecode(
            bytecode.to_vec(),
            u16::try_from(arguments.len()).unwrap(),
            u16::try_from(locals.len()).unwrap(),
        );
        let code = Tier2Compiler::host(1)
            .publish_for_test(&verified, 1)
            .unwrap_or_else(|error| panic!("{bytecode:?} did not lower: {error:?}"));
        let mut frame = SyntheticFrame::new(arguments, locals.len(), 8);
        for (index, value) in locals.iter().enumerate() {
            frame.set_local(index, *value);
        }
        frame.set_bytecode(bytecode);
        let outcome = unsafe { frame.call(&code) };
        let start = frame.bytecode_start() as usize;
        Run {
            kind: outcome.exit.kind,
            map: outcome.exit.reserved,
            resume: (!outcome.exit.resume_pc.is_null())
                .then(|| outcome.exit.resume_pc as usize - start),
            result: outcome.result,
        }
    }

    fn done(bytecode: &[u8], arguments: &[JSValueRepr], locals: &[JSValueRepr]) -> JSValueRepr {
        let run = run(bytecode, arguments, locals);
        assert_eq!(
            run.kind,
            qjs::JSJitExitKind_JS_JIT_EXIT_DONE,
            "{bytecode:?} exited early at {:?}",
            run.resume
        );
        run.result
    }

    fn value(bytecode: &[u8]) -> JSValueRepr {
        done(bytecode, &[], &[])
    }

    /// The instruction at `pc` must leave through an exact deoptimization
    /// exit that names its guard site and resumes at that very instruction.
    fn deopts_at(bytecode: &[u8], arguments: &[JSValueRepr], locals: &[JSValueRepr], pc: usize) {
        let run = run(bytecode, arguments, locals);
        assert_eq!(
            run.kind,
            qjs::JSJitExitKind_JS_JIT_EXIT_DEOPT,
            "{bytecode:?} result={:?}",
            run.result
        );
        assert_eq!(run.resume, Some(pc), "{bytecode:?}");
        assert_ne!(
            run.map, 0,
            "deopt exits name their guard site: {bytecode:?}"
        );
    }

    /// A deopt exit whose operand aliases a borrowed argument asks the
    /// runtime to materialize an owner. The synthetic runtime API reports
    /// that helper as unavailable, so the observable outcome is the
    /// exception exit rather than the fast path's DONE.
    fn leaves_fast_path(bytecode: &[u8], arguments: &[JSValueRepr]) {
        let run = run(bytecode, arguments, &[]);
        assert_eq!(
            run.kind,
            qjs::JSJitExitKind_JS_JIT_EXIT_EXCEPTION,
            "{bytecode:?} result={:?}",
            run.result
        );
    }

    const fn int(value: i32) -> JSValueRepr {
        JSValueRepr::int32(value)
    }
    const fn float(value: f64) -> JSValueRepr {
        JSValueRepr::float64(value)
    }
    const fn boolean(value: bool) -> JSValueRepr {
        JSValueRepr::new(value as u64, qjs::JS_TAG_BOOL as i64)
    }
    const fn null() -> JSValueRepr {
        JSValueRepr::new(0, qjs::JS_TAG_NULL as i64)
    }
    const fn string_like() -> JSValueRepr {
        JSValueRepr::new(0x1000, qjs::JS_TAG_STRING as i64)
    }
    fn push_i32(value: i32) -> Vec<u8> {
        let mut bytes = vec![qjs::QJS_JIT_OP_PUSH_I32];
        bytes.extend(value.to_le_bytes());
        bytes
    }
    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }
    /// `3 / 2` as a Float64 operand.
    const ONE_AND_A_HALF: [u8; 5] = [
        qjs::QJS_JIT_OP_PUSH_I8,
        3,
        qjs::QJS_JIT_OP_PUSH_I8,
        2,
        qjs::QJS_JIT_OP_DIV,
    ];
    /// `0 / 0` as a NaN operand.
    const NAN: [u8; 3] = [
        qjs::QJS_JIT_OP_PUSH_0,
        qjs::QJS_JIT_OP_PUSH_0,
        qjs::QJS_JIT_OP_DIV,
    ];

    #[test]
    fn constants_push_exact_tagged_values() {
        assert_eq!(
            value(&[qjs::QJS_JIT_OP_PUSH_MINUS1, opcode::RETURN]),
            int(-1)
        );
        assert_eq!(value(&[qjs::QJS_JIT_OP_NULL, opcode::RETURN]), null());
        assert_eq!(
            value(&[qjs::QJS_JIT_OP_PUSH_TRUE, opcode::RETURN]),
            boolean(true)
        );
        assert_eq!(
            value(&[qjs::QJS_JIT_OP_PUSH_FALSE, opcode::RETURN]),
            boolean(false)
        );
        assert_eq!(
            value(&[qjs::QJS_JIT_OP_NOP, opcode::PUSH_I8, 9, opcode::RETURN]),
            int(9)
        );
    }

    #[test]
    fn stack_shuffles_permute_values_exactly_like_the_interpreter() {
        // (opcode, values consumed, sources of the values pushed back) as in
        // QuickJS's interpreter and the verifier's `copied_stack_values`.
        let shuffles: &[(u8, usize, &[usize])] = &[
            (qjs::QJS_JIT_OP_NIP, 2, &[1]),
            (qjs::QJS_JIT_OP_NIP1, 3, &[1, 2]),
            (qjs::QJS_JIT_OP_DUP, 1, &[0, 0]),
            (qjs::QJS_JIT_OP_DUP1, 2, &[0, 0, 1]),
            (qjs::QJS_JIT_OP_DUP2, 2, &[0, 1, 0, 1]),
            (qjs::QJS_JIT_OP_DUP3, 3, &[0, 1, 2, 0, 1, 2]),
            (qjs::QJS_JIT_OP_INSERT2, 2, &[1, 0, 1]),
            (qjs::QJS_JIT_OP_INSERT3, 3, &[2, 0, 1, 2]),
            (qjs::QJS_JIT_OP_INSERT4, 4, &[3, 0, 1, 2, 3]),
            (qjs::QJS_JIT_OP_PERM3, 3, &[1, 0, 2]),
            (qjs::QJS_JIT_OP_PERM4, 4, &[2, 0, 1, 3]),
            (qjs::QJS_JIT_OP_PERM5, 5, &[3, 0, 1, 2, 4]),
            (qjs::QJS_JIT_OP_SWAP, 2, &[1, 0]),
            (qjs::QJS_JIT_OP_SWAP2, 4, &[2, 3, 0, 1]),
            (qjs::QJS_JIT_OP_ROT3L, 3, &[1, 2, 0]),
            (qjs::QJS_JIT_OP_ROT3R, 3, &[2, 0, 1]),
            (qjs::QJS_JIT_OP_ROT4L, 4, &[1, 2, 3, 0]),
            (qjs::QJS_JIT_OP_ROT5L, 5, &[1, 2, 3, 4, 0]),
        ];
        let arguments = [int(10), int(20), int(30), int(40), int(50)];
        for (shuffle, take, order) in shuffles {
            for (position, source) in order.iter().enumerate() {
                let mut bytecode = Vec::new();
                for index in 0..*take {
                    bytecode.extend([opcode::GET_ARG, index as u8, 0]);
                }
                bytecode.push(*shuffle);
                // Drop everything above the slot under test, then return it.
                bytecode.extend(std::iter::repeat_n(
                    opcode::DROP,
                    order.len() - 1 - position,
                ));
                bytecode.push(opcode::RETURN);
                assert_eq!(
                    done(&bytecode, &arguments[..*take], &[]),
                    arguments[*source],
                    "opcode {shuffle} slot {position}"
                );
            }
        }
    }

    #[test]
    fn get_loc0_loc1_pushes_both_locals_in_order() {
        let locals = [int(7), int(9)];
        assert_eq!(
            done(
                &[qjs::QJS_JIT_OP_GET_LOC0_LOC1, opcode::RETURN],
                &[],
                &locals
            ),
            int(9)
        );
        assert_eq!(
            done(
                &[qjs::QJS_JIT_OP_GET_LOC0_LOC1, opcode::DROP, opcode::RETURN],
                &[],
                &locals
            ),
            int(7)
        );
    }

    #[test]
    fn argument_and_local_stores_redefine_the_frame_slot() {
        let arguments = [int(1), int(2)];
        assert_eq!(
            done(
                &[
                    opcode::PUSH_I8,
                    5,
                    qjs::QJS_JIT_OP_PUT_ARG,
                    0,
                    0,
                    qjs::QJS_JIT_OP_GET_ARG0,
                    opcode::RETURN
                ],
                &arguments,
                &[]
            ),
            int(5)
        );
        assert_eq!(
            done(
                &[
                    opcode::PUSH_I8,
                    6,
                    qjs::QJS_JIT_OP_PUT_ARG1,
                    qjs::QJS_JIT_OP_GET_ARG1,
                    opcode::RETURN
                ],
                &arguments,
                &[]
            ),
            int(6)
        );
        // set_* keeps the stored value on the stack.
        assert_eq!(
            done(
                &[
                    opcode::PUSH_I8,
                    7,
                    qjs::QJS_JIT_OP_SET_ARG,
                    0,
                    0,
                    opcode::DROP,
                    qjs::QJS_JIT_OP_GET_ARG0,
                    opcode::RETURN
                ],
                &arguments,
                &[]
            ),
            int(7)
        );
        assert_eq!(
            done(
                &[opcode::PUSH_I8, 7, qjs::QJS_JIT_OP_SET_ARG0, opcode::RETURN],
                &arguments,
                &[]
            ),
            int(7)
        );
        let locals = [JSValueRepr::undefined(); 3];
        assert_eq!(
            done(
                &[
                    opcode::PUSH_I8,
                    8,
                    qjs::QJS_JIT_OP_SET_LOC,
                    0,
                    0,
                    opcode::DROP,
                    qjs::QJS_JIT_OP_GET_LOC0,
                    opcode::RETURN
                ],
                &[],
                &locals
            ),
            int(8)
        );
        assert_eq!(
            done(
                &[
                    opcode::PUSH_I8,
                    9,
                    qjs::QJS_JIT_OP_SET_LOC8,
                    1,
                    opcode::DROP,
                    qjs::QJS_JIT_OP_GET_LOC1,
                    opcode::RETURN
                ],
                &[],
                &locals
            ),
            int(9)
        );
        assert_eq!(
            done(
                &[
                    opcode::PUSH_I8,
                    3,
                    qjs::QJS_JIT_OP_SET_LOC2,
                    opcode::DROP,
                    qjs::QJS_JIT_OP_GET_LOC2,
                    opcode::RETURN
                ],
                &[],
                &locals
            ),
            int(3)
        );
        // A stale alias of the redefined slot still yields the old value.
        assert_eq!(
            done(
                &[
                    qjs::QJS_JIT_OP_GET_ARG0,
                    opcode::PUSH_I8,
                    4,
                    qjs::QJS_JIT_OP_PUT_ARG0,
                    opcode::RETURN
                ],
                &arguments,
                &[]
            ),
            int(1)
        );
    }

    #[test]
    fn goto_with_a_32_bit_label_branches_exactly() {
        // 0: get_arg0  1: if_false8 -> 10  3: push_i8 1  5: goto -> 12
        // 10: push_i8 2  12: return
        let bytecode = [
            qjs::QJS_JIT_OP_GET_ARG0,
            opcode::IF_FALSE8,
            8,
            opcode::PUSH_I8,
            1,
            qjs::QJS_JIT_OP_GOTO,
            6,
            0,
            0,
            0,
            opcode::PUSH_I8,
            2,
            opcode::RETURN,
        ];
        assert_eq!(done(&bytecode, &[int(1)], &[]), int(1));
        assert_eq!(done(&bytecode, &[int(0)], &[]), int(2));
    }

    #[test]
    fn negation_and_plus_compute_exact_numeric_results() {
        assert_eq!(
            value(&[opcode::PUSH_I8, 5, qjs::QJS_JIT_OP_NEG, opcode::RETURN]),
            int(-5)
        );
        assert_eq!(
            value(&[qjs::QJS_JIT_OP_PUSH_0, qjs::QJS_JIT_OP_NEG, opcode::RETURN]),
            float(-0.0)
        );
        assert_eq!(
            value(&cat(&[
                &push_i32(i32::MIN),
                &[qjs::QJS_JIT_OP_NEG, opcode::RETURN]
            ])),
            float(2_147_483_648.0)
        );
        assert_eq!(
            value(&cat(&[
                &ONE_AND_A_HALF,
                &[qjs::QJS_JIT_OP_NEG, opcode::RETURN]
            ])),
            float(-1.5)
        );
        deopts_at(
            &[
                qjs::QJS_JIT_OP_PUSH_TRUE,
                qjs::QJS_JIT_OP_NEG,
                opcode::RETURN,
            ],
            &[],
            &[],
            1,
        );
        assert_eq!(
            value(&[opcode::PUSH_I8, 5, opcode::PLUS, opcode::RETURN]),
            int(5)
        );
        assert_eq!(
            value(&cat(&[&ONE_AND_A_HALF, &[opcode::PLUS, opcode::RETURN]])),
            float(1.5)
        );
        deopts_at(
            &[qjs::QJS_JIT_OP_NULL, opcode::PLUS, opcode::RETURN],
            &[],
            &[],
            1,
        );
    }

    #[test]
    fn bitwise_not_applies_to_int32_and_converts_floats_exactly() {
        assert_eq!(
            value(&[opcode::PUSH_I8, 1, qjs::QJS_JIT_OP_NOT, opcode::RETURN]),
            int(-2)
        );
        assert_eq!(
            value(&cat(&[
                &ONE_AND_A_HALF,
                &[qjs::QJS_JIT_OP_NOT, opcode::RETURN]
            ])),
            int(-2)
        );
        assert_eq!(
            value(&cat(&[
                &ONE_AND_A_HALF,
                &[qjs::QJS_JIT_OP_NEG, qjs::QJS_JIT_OP_NOT, opcode::RETURN]
            ])),
            int(0)
        );
        assert_eq!(
            value(&cat(&[&NAN, &[qjs::QJS_JIT_OP_NOT, opcode::RETURN]])),
            int(-1)
        );
        // 2^31 - 1 + 2^31 - 1 = 4294967294 wraps to -2 under ToInt32.
        assert_eq!(
            value(&cat(&[
                &push_i32(i32::MAX),
                &push_i32(i32::MAX),
                &[opcode::ADD, qjs::QJS_JIT_OP_NOT, opcode::RETURN]
            ])),
            int(1)
        );
        deopts_at(
            &[
                qjs::QJS_JIT_OP_PUSH_TRUE,
                qjs::QJS_JIT_OP_NOT,
                opcode::RETURN,
            ],
            &[],
            &[],
            1,
        );
    }

    #[test]
    fn logical_not_computes_truthiness_natively_for_primitives() {
        let cases: &[(&[u8], bool)] = &[
            (&[qjs::QJS_JIT_OP_PUSH_0], true),
            (&[opcode::PUSH_I8, 3], false),
            (&[opcode::PUSH_I8, 0x80], false),
            (&ONE_AND_A_HALF, false),
            (&NAN, true),
            (&[qjs::QJS_JIT_OP_PUSH_0, qjs::QJS_JIT_OP_NEG], true),
            (&[qjs::QJS_JIT_OP_NULL], true),
            (&[opcode::PUSH_UNDEFINED], true),
            (&[qjs::QJS_JIT_OP_PUSH_TRUE], false),
            (&[qjs::QJS_JIT_OP_PUSH_FALSE], true),
        ];
        for (operand, expected) in cases {
            let bytecode = cat(&[operand, &[qjs::QJS_JIT_OP_LNOT, opcode::RETURN]]);
            assert_eq!(value(&bytecode), boolean(*expected), "{operand:?}");
        }
        leaves_fast_path(
            &[
                qjs::QJS_JIT_OP_GET_ARG0,
                qjs::QJS_JIT_OP_LNOT,
                opcode::RETURN,
            ],
            &[string_like()],
        );
    }

    #[test]
    fn is_undefined_and_is_null_test_the_tag() {
        assert_eq!(
            value(&[
                opcode::PUSH_UNDEFINED,
                qjs::QJS_JIT_OP_IS_UNDEFINED,
                opcode::RETURN
            ]),
            boolean(true)
        );
        assert_eq!(
            value(&[
                qjs::QJS_JIT_OP_NULL,
                qjs::QJS_JIT_OP_IS_UNDEFINED,
                opcode::RETURN
            ]),
            boolean(false)
        );
        assert_eq!(
            value(&[
                qjs::QJS_JIT_OP_NULL,
                qjs::QJS_JIT_OP_IS_NULL,
                opcode::RETURN
            ]),
            boolean(true)
        );
        assert_eq!(
            value(&[
                qjs::QJS_JIT_OP_PUSH_0,
                qjs::QJS_JIT_OP_IS_NULL,
                opcode::RETURN
            ]),
            boolean(false)
        );
    }

    #[test]
    fn decrement_family_mirrors_increment_with_exact_overflow() {
        assert_eq!(
            value(&[opcode::PUSH_I8, 5, qjs::QJS_JIT_OP_DEC, opcode::RETURN]),
            int(4)
        );
        assert_eq!(
            value(&cat(&[
                &push_i32(i32::MIN),
                &[qjs::QJS_JIT_OP_DEC, opcode::RETURN]
            ])),
            float(-2_147_483_649.0)
        );
        assert_eq!(
            value(&cat(&[
                &push_i32(i32::MAX),
                &[qjs::QJS_JIT_OP_INC, opcode::RETURN]
            ])),
            float(2_147_483_648.0)
        );
        assert_eq!(
            value(&cat(&[
                &ONE_AND_A_HALF,
                &[qjs::QJS_JIT_OP_DEC, opcode::RETURN]
            ])),
            float(0.5)
        );
        assert_eq!(
            value(&[opcode::PUSH_I8, 5, qjs::QJS_JIT_OP_POST_DEC, opcode::RETURN]),
            int(4)
        );
        assert_eq!(
            value(&[
                opcode::PUSH_I8,
                5,
                qjs::QJS_JIT_OP_POST_DEC,
                opcode::DROP,
                opcode::RETURN
            ]),
            int(5)
        );
        assert_eq!(
            value(&[opcode::PUSH_I8, 5, qjs::QJS_JIT_OP_POST_INC, opcode::RETURN]),
            int(6)
        );
        deopts_at(
            &[
                qjs::QJS_JIT_OP_PUSH_TRUE,
                qjs::QJS_JIT_OP_DEC,
                opcode::RETURN,
            ],
            &[],
            &[],
            1,
        );
        deopts_at(
            &[
                qjs::QJS_JIT_OP_NULL,
                qjs::QJS_JIT_OP_POST_INC,
                opcode::RETURN,
            ],
            &[],
            &[],
            1,
        );
    }

    #[test]
    fn local_arithmetic_updates_the_slot_in_place() {
        let read_local0 = [qjs::QJS_JIT_OP_GET_LOC0, opcode::RETURN];
        assert_eq!(
            done(&cat(&[&[opcode::INC_LOC, 0], &read_local0]), &[], &[int(5)]),
            int(6)
        );
        assert_eq!(
            done(&cat(&[&[opcode::DEC_LOC, 0], &read_local0]), &[], &[int(5)]),
            int(4)
        );
        assert_eq!(
            done(
                &cat(&[&[opcode::PUSH_I8, 10, opcode::ADD_LOC, 0], &read_local0]),
                &[],
                &[int(5)]
            ),
            int(15)
        );
        assert_eq!(
            done(
                &cat(&[&[opcode::INC_LOC, 0], &read_local0]),
                &[],
                &[int(i32::MAX)]
            ),
            float(2_147_483_648.0)
        );
        assert_eq!(
            done(
                &cat(&[&[opcode::DEC_LOC, 0], &read_local0]),
                &[],
                &[int(i32::MIN)]
            ),
            float(-2_147_483_649.0)
        );
        assert_eq!(
            done(
                &cat(&[&[opcode::PUSH_I8, 1, opcode::ADD_LOC, 0], &read_local0]),
                &[],
                &[float(0.5)]
            ),
            float(1.5)
        );
        // Redefining local 1 leaves local 0 and the stack alias untouched.
        assert_eq!(
            done(
                &[qjs::QJS_JIT_OP_GET_LOC0, opcode::INC_LOC, 1, opcode::RETURN],
                &[],
                &[int(3), int(4)]
            ),
            int(3)
        );
        deopts_at(
            &cat(&[
                &[qjs::QJS_JIT_OP_PUSH_TRUE, opcode::ADD_LOC, 0],
                &read_local0,
            ]),
            &[],
            &[int(1)],
            1,
        );
        deopts_at(
            &cat(&[&[opcode::INC_LOC, 0], &read_local0]),
            &[],
            &[null()],
            0,
        );
        deopts_at(
            &cat(&[&[opcode::DEC_LOC, 0], &read_local0]),
            &[],
            &[boolean(true)],
            0,
        );
    }

    #[test]
    fn shifts_mask_the_count_and_unsigned_shift_renormalizes() {
        assert_eq!(
            value(&[
                opcode::PUSH_I8,
                1,
                opcode::PUSH_I8,
                33,
                qjs::QJS_JIT_OP_SHL,
                opcode::RETURN
            ]),
            int(2)
        );
        assert_eq!(
            value(&[
                opcode::PUSH_I8,
                1,
                opcode::PUSH_I8,
                32,
                qjs::QJS_JIT_OP_SHL,
                opcode::RETURN
            ]),
            int(1)
        );
        assert_eq!(
            value(&cat(&[
                &push_i32(-8),
                &[opcode::PUSH_I8, 33, qjs::QJS_JIT_OP_SAR, opcode::RETURN]
            ])),
            int(-4)
        );
        assert_eq!(
            value(&[
                qjs::QJS_JIT_OP_PUSH_MINUS1,
                qjs::QJS_JIT_OP_PUSH_0,
                qjs::QJS_JIT_OP_SHR,
                opcode::RETURN
            ]),
            float(4_294_967_295.0)
        );
        // A constant count of 32 masks to zero: still a Float64 result.
        assert_eq!(
            value(&[
                qjs::QJS_JIT_OP_PUSH_MINUS1,
                opcode::PUSH_I8,
                32,
                qjs::QJS_JIT_OP_SHR,
                opcode::RETURN
            ]),
            float(4_294_967_295.0)
        );
        assert_eq!(
            value(&[
                qjs::QJS_JIT_OP_PUSH_MINUS1,
                opcode::PUSH_I8,
                1,
                qjs::QJS_JIT_OP_SHR,
                opcode::RETURN
            ]),
            int(i32::MAX)
        );
        assert_eq!(
            value(&[
                opcode::PUSH_I8,
                8,
                opcode::PUSH_I8,
                33,
                qjs::QJS_JIT_OP_SHR,
                opcode::RETURN
            ]),
            int(4)
        );
        assert_eq!(
            value(&[
                opcode::PUSH_I8,
                6,
                opcode::PUSH_I8,
                3,
                qjs::QJS_JIT_OP_XOR,
                opcode::RETURN
            ]),
            int(5)
        );
        deopts_at(
            &[
                opcode::PUSH_I8,
                1,
                qjs::QJS_JIT_OP_PUSH_TRUE,
                qjs::QJS_JIT_OP_SHL,
                opcode::RETURN,
            ],
            &[],
            &[],
            3,
        );
        deopts_at(
            &cat(&[
                &ONE_AND_A_HALF,
                &[opcode::PUSH_I8, 1, qjs::QJS_JIT_OP_SHR, opcode::RETURN],
            ]),
            &[],
            &[],
            7,
        );
    }

    #[test]
    fn modulo_computes_only_exact_int32_remainders() {
        assert_eq!(
            value(&[
                opcode::PUSH_I8,
                7,
                opcode::PUSH_I8,
                3,
                qjs::QJS_JIT_OP_MOD,
                opcode::RETURN
            ]),
            int(1)
        );
        assert_eq!(
            value(&[
                opcode::PUSH_I8,
                7,
                qjs::QJS_JIT_OP_PUSH_MINUS1,
                qjs::QJS_JIT_OP_MOD,
                opcode::RETURN
            ]),
            int(0)
        );
        assert_eq!(
            value(&cat(&[
                &[opcode::PUSH_I8, 7],
                &push_i32(-3),
                &[qjs::QJS_JIT_OP_MOD, opcode::RETURN]
            ])),
            int(1)
        );
        assert_eq!(
            value(&[
                qjs::QJS_JIT_OP_PUSH_0,
                opcode::PUSH_I8,
                5,
                qjs::QJS_JIT_OP_MOD,
                opcode::RETURN
            ]),
            int(0)
        );
        // -4 % 2 is -0, x % 0 is NaN and INT32_MIN % -1 must not trap: all
        // of them resume in the interpreter at the `mod` instruction.
        deopts_at(
            &cat(&[
                &push_i32(-4),
                &[opcode::PUSH_I8, 2, qjs::QJS_JIT_OP_MOD, opcode::RETURN],
            ]),
            &[],
            &[],
            7,
        );
        deopts_at(
            &[
                opcode::PUSH_I8,
                5,
                qjs::QJS_JIT_OP_PUSH_0,
                qjs::QJS_JIT_OP_MOD,
                opcode::RETURN,
            ],
            &[],
            &[],
            3,
        );
        deopts_at(
            &cat(&[
                &push_i32(i32::MIN),
                &[
                    qjs::QJS_JIT_OP_PUSH_MINUS1,
                    qjs::QJS_JIT_OP_MOD,
                    opcode::RETURN,
                ],
            ]),
            &[],
            &[],
            6,
        );
        deopts_at(
            &cat(&[
                &ONE_AND_A_HALF,
                &[opcode::PUSH_I8, 1, qjs::QJS_JIT_OP_MOD, opcode::RETURN],
            ]),
            &[],
            &[],
            7,
        );
    }

    #[test]
    fn equality_opcodes_compute_exact_results_and_deopt_on_coercions() {
        let one = [opcode::PUSH_I8, 1];
        let two = [opcode::PUSH_I8, 2];
        let two_float = [opcode::PUSH_I8, 4, opcode::PUSH_I8, 2, qjs::QJS_JIT_OP_DIV];
        let negative_zero = [qjs::QJS_JIT_OP_PUSH_0, qjs::QJS_JIT_OP_NEG];
        let zero = [qjs::QJS_JIT_OP_PUSH_0];
        let null_value = [qjs::QJS_JIT_OP_NULL];
        let undefined_value = [opcode::PUSH_UNDEFINED];
        let yes = [qjs::QJS_JIT_OP_PUSH_TRUE];
        let no = [qjs::QJS_JIT_OP_PUSH_FALSE];
        let cases: &[(&[u8], &[u8], u8, bool)] = &[
            (&one, &one, qjs::QJS_JIT_OP_STRICT_EQ, true),
            (&one, &one, qjs::QJS_JIT_OP_STRICT_NEQ, false),
            (&one, &two, qjs::QJS_JIT_OP_EQ, false),
            (&one, &two, qjs::QJS_JIT_OP_NEQ, true),
            (&two, &two_float, qjs::QJS_JIT_OP_STRICT_EQ, true),
            (&one, &ONE_AND_A_HALF, qjs::QJS_JIT_OP_EQ, false),
            (&negative_zero, &zero, qjs::QJS_JIT_OP_STRICT_EQ, true),
            (&null_value, &undefined_value, qjs::QJS_JIT_OP_EQ, true),
            (&undefined_value, &null_value, qjs::QJS_JIT_OP_NEQ, false),
            (
                &null_value,
                &undefined_value,
                qjs::QJS_JIT_OP_STRICT_EQ,
                false,
            ),
            (
                &null_value,
                &undefined_value,
                qjs::QJS_JIT_OP_STRICT_NEQ,
                true,
            ),
            (&null_value, &null_value, qjs::QJS_JIT_OP_STRICT_EQ, true),
            (&undefined_value, &undefined_value, qjs::QJS_JIT_OP_EQ, true),
            (&yes, &yes, qjs::QJS_JIT_OP_STRICT_EQ, true),
            (&yes, &no, qjs::QJS_JIT_OP_EQ, false),
            (&yes, &no, qjs::QJS_JIT_OP_STRICT_NEQ, true),
            // Differing primitive tags never coerce under `===`, and
            // `null`/`undefined` only ever equal each other under `==`.
            (&yes, &one, qjs::QJS_JIT_OP_STRICT_EQ, false),
            (&yes, &one, qjs::QJS_JIT_OP_STRICT_NEQ, true),
            (&null_value, &zero, qjs::QJS_JIT_OP_EQ, false),
            (&null_value, &zero, qjs::QJS_JIT_OP_NEQ, true),
            (&undefined_value, &ONE_AND_A_HALF, qjs::QJS_JIT_OP_EQ, false),
            (&undefined_value, &zero, qjs::QJS_JIT_OP_STRICT_NEQ, true),
            (&no, &zero, qjs::QJS_JIT_OP_STRICT_EQ, false),
        ];
        for (lhs, rhs, operation, expected) in cases {
            let bytecode = cat(&[lhs, rhs, &[*operation, opcode::RETURN]]);
            assert_eq!(value(&bytecode), boolean(*expected), "{bytecode:?}");
        }
        for operation in [
            qjs::QJS_JIT_OP_EQ,
            qjs::QJS_JIT_OP_NEQ,
            qjs::QJS_JIT_OP_STRICT_EQ,
            qjs::QJS_JIT_OP_STRICT_NEQ,
        ] {
            // NaN is unequal to itself under every operator.
            let bytecode = cat(&[&NAN, &[opcode::DUP, operation, opcode::RETURN]]);
            let expected = matches!(operation, qjs::QJS_JIT_OP_NEQ | qjs::QJS_JIT_OP_STRICT_NEQ);
            assert_eq!(value(&bytecode), boolean(expected), "{bytecode:?}");
            // Loose equality coerces a boolean to a number; strings (and
            // every other heap tag) always belong to the interpreter.
            if matches!(operation, qjs::QJS_JIT_OP_EQ | qjs::QJS_JIT_OP_NEQ) {
                deopts_at(
                    &cat(&[&yes, &one, &[operation, opcode::RETURN]]),
                    &[],
                    &[],
                    3,
                );
                deopts_at(
                    &cat(&[&zero, &no, &[operation, opcode::RETURN]]),
                    &[],
                    &[],
                    2,
                );
            }
            leaves_fast_path(
                &[
                    qjs::QJS_JIT_OP_GET_ARG0,
                    qjs::QJS_JIT_OP_GET_ARG1,
                    operation,
                    opcode::RETURN,
                ],
                &[string_like(), string_like()],
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The designated int-arith benchmark kernel: global lookups, a kept-receiver
// property load, native callees and a tail call around unboxed Int32 loops.
// ---------------------------------------------------------------------------

const INT_ARITH_KERNEL: &str = include_str!("../../benchmarks/scripts/quickjs-int-arith.js");

#[test]
fn int_arith_kernel_inner_loop_is_an_unboxed_int32_loop_around_native_calls() {
    let fixture = SnapshotFixture::compile(&format!("{INT_ARITH_KERNEL}\nworkload"));
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let names = verified
        .instructions()
        .iter()
        .map(|instruction| instruction.opcode().name())
        .collect::<Vec<_>>();
    for required in ["get_var", "get_field2", "call_method", "tail_call", "mul"] {
        assert!(names.contains(&required), "{required} missing: {names:?}");
    }
    let ir = OptimizedIr::translate(&verified, 81)
        .unwrap_or_else(|error| panic!("kernel IR translation failed: {error:?}"));
    // The innermost loop is the last loop header; its body runs up to the
    // block that branches back to it. No node in it may reach a helper.
    let inner = ir
        .blocks()
        .iter()
        .rposition(|block| block.is_loop_header())
        .expect("nested loops");
    let header_pc = ir.blocks()[inner].start_pc();
    let back_edge = ir
        .blocks()
        .iter()
        .rposition(|block| block.successors().contains(&header_pc))
        .expect("inner loop back edge");
    assert!(back_edge >= inner);
    for node in ir.blocks()[inner..=back_edge]
        .iter()
        .flat_map(|block| block.nodes())
        .map(|id| &ir.nodes()[*id as usize])
    {
        assert_ne!(
            node.effect(),
            OptimizedEffect::Reentrant,
            "helper-backed node inside the inner loop: {node:?}"
        );
    }
    assert!(ir.blocks()[inner..=back_edge]
        .iter()
        .flat_map(|block| block.nodes())
        .any(|id| matches!(
            ir.nodes()[*id as usize].kind(),
            OptimizedNodeKind::Bytecode { opcode } if &**opcode == "mul"
        )));

    // Generic lowering (no feedback) must accept the whole kernel.
    Tier2Compiler::host(81)
        .lower_for_test(&verified, 81)
        .unwrap_or_else(|error| panic!("kernel did not lower: {error:?}"));

    // Stable Int32 feedback on the products and sums selects the checked
    // Int32 paths even though the kernel returns a string.
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..8 {
        for instruction in verified
            .instructions()
            .iter()
            .filter(|instruction| matches!(instruction.opcode().name(), "add" | "mul"))
        {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                Default::default(),
            );
        }
    }
    let clif = Tier2Compiler::host(81)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(81))
        .unwrap_or_else(|error| panic!("kernel did not lower with feedback: {error:?}"));
    assert!(clif.contains("smul_overflow"), "{clif}");
    assert!(clif.contains("sadd_overflow"), "{clif}");
    assert!(
        !clif.contains("fmul"),
        "the inner product must stay Int32: {clif}"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn production_tier2_runs_the_int_arith_kernel_end_to_end() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};

    let expected = {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            ctx.eval::<(), _>(INT_ARITH_KERNEL).unwrap();
            ctx.eval::<String, _>("workload(4096)").unwrap()
        })
    };
    assert_eq!(expected, "10650672000");

    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(JitTierPolicy::Optimize)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    // Warm through one predefined caller: evaluating a fresh script per
    // iteration would queue a throwaway compilation ahead of the kernel's
    // Tier 2 job every time and starve it under slow instrumented builds.
    context.with(|ctx| {
        ctx.eval::<(), _>(INT_ARITH_KERNEL).unwrap();
        ctx.eval::<(), _>("globalThis.__warm=function(){return workload(4096)}")
            .unwrap();
    });
    let run = || {
        context.with(|ctx| {
            let warm: rquickjs::Function<'_> = ctx.globals().get("__warm").unwrap();
            warm.call::<_, String>(())
                .unwrap_or_else(|error| panic!("{error} {:?}", ctx.catch()))
        })
    };
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 600 } else { 120 });
    while std::time::Instant::now() < deadline {
        assert_eq!(run(), expected);
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let entered = jit.metrics();
    assert!(entered.tier2_entries > 0, "{entered:?}");
    for _ in 0..8 {
        assert_eq!(run(), expected);
        jit.poll();
    }
    let metrics = jit.metrics();
    assert!(metrics.tier2_entries > entered.tier2_entries, "{metrics:?}");
    assert_eq!(metrics.native_fallbacks, 0, "{metrics:?}");
    assert_eq!(metrics.native_retries, 0, "{metrics:?}");
    assert_eq!(metrics.deopts, 0, "{metrics:?}");
}

// ---------------------------------------------------------------------------
// Ownership of borrowed argument/local aliases in interpreter-owned storage.

fn heap_argument_feedback(
    verified: &rquickjs_jit::bytecode::VerifiedFunction,
    key: FunctionKey,
) -> FeedbackTable {
    let return_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "return")
        .unwrap()
        .pc();
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
        for instruction in verified.instructions().iter().filter(|instruction| {
            matches!(instruction.opcode().name(), "add" | "sub" | "mul" | "div")
        }) {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    feedback
}

#[test]
fn storing_a_proven_heap_argument_alias_into_a_local_fails_closed() {
    // Deoptimization maps carry identity recipes only, so a borrowed heap
    // alias spilled into a different interpreter-owned slot would hand the
    // interpreter an unowned reference. Such functions stay on Tier 1.
    let fixture = SnapshotFixture::compile(
        "(function(n,s,p){ let q=p; let t=0; for(let i=0;i<n;i++){ t = t + q.x } return t })",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let feedback = heap_argument_feedback(&verified, key);
    let error = Tier2Compiler::host(171)
        .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(171))
        .expect_err("heap alias store must not lower");
    assert!(
        matches!(
            error,
            rquickjs_jit::compiler::CompileFailure::UnsupportedOpcode
        ),
        "{error:?}"
    );
}

#[test]
fn storing_an_unproven_alias_into_a_local_guards_the_tag_before_the_spill() {
    // Without feedback every argument is `Any`: the store keeps compiling but
    // checks at run time that the value is not reference counted, and
    // deoptimizes exactly at the store otherwise.
    let fixture =
        SnapshotFixture::compile("(function(n,z){let s=z;for(let i=z;i<n;i++)s=s+i;return s})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 172).unwrap();
    let store_guards = ir
        .nodes()
        .iter()
        .filter(|node| {
            matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.starts_with("put_loc"))
        })
        .filter(|node| node.deopt_guard().is_some())
        .count();
    assert!(store_guards >= 2, "every local store carries a guard site");
    let clif = Tier2Compiler::host(172)
        .lower_for_test(&verified, 172)
        .unwrap();
    assert!(
        clif.contains("icmp_imm sge") || clif.contains("icmp_imm.i64 sge"),
        "non-refcounted tag guard missing: {clif}"
    );
}

// ---------------------------------------------------------------------------
// Counted-loop increments whose overflow exit is provably unnecessary.

fn int32_loop_clif(source: &str) -> String {
    int32_loop_code(source, false)
}

fn int32_loop_code(source: &str, machine: bool) -> String {
    scalar_code_with_arguments(source, machine, &[ObservedType::Int32, ObservedType::Int32])
}

fn scalar_code_with_arguments(source: &str, machine: bool, arguments: &[ObservedType]) -> String {
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
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(key, arguments);
        for instruction in verified.instructions().iter().filter(|instruction| {
            matches!(instruction.opcode().name(), "add" | "sub" | "mul" | "div")
        }) {
            feedback.observe_binary(
                key,
                instruction.pc(),
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                Default::default(),
            );
        }
        feedback.observe_return(key, return_pc, ObservedType::Int32);
    }
    let compiler = Tier2Compiler::host(181);
    if machine {
        compiler
            .machine_with_feedback_for_test(&verified, key, &feedback.snapshot(181))
            .unwrap()
    } else {
        compiler
            .lower_with_feedback_for_test(&verified, key, &feedback.snapshot(181))
            .unwrap()
    }
}

#[test]
#[cfg(target_arch = "aarch64")]
fn mixed_entry_facts_remove_numeric_operand_checks() {
    let machine = scalar_code_with_arguments(
        "(function(a,b,object){return a+b})",
        true,
        &[
            ObservedType::Int32,
            ObservedType::Int32,
            ObservedType::Object,
        ],
    );
    assert!(
        machine
            .lines()
            .filter(|line| line.trim().starts_with("cbnz "))
            .count()
            <= 2,
        "mixed entry guards already prove both numeric operands: {machine}"
    );
    assert!(
        machine.contains("adds "),
        "overflow must still be checked: {machine}"
    );
}

#[test]
#[cfg(target_arch = "aarch64")]
fn int32_entry_facts_remove_repeated_leaf_operand_checks() {
    let machine = int32_loop_code("(function(a,b){return a+b})", true);
    assert!(
        machine
            .lines()
            .filter(|line| line.trim().starts_with("subs xzr,"))
            .count()
            <= 2,
        "the entry guard already checks both argument tags: {machine}"
    );
    assert!(
        machine
            .lines()
            .filter(|line| line.trim().starts_with("cbnz "))
            .count()
            <= 2,
        "only the entry and overflow branches are needed: {machine}"
    );
    assert!(
        machine.contains("adds "),
        "overflow checking remains: {machine}"
    );
}

#[test]
#[cfg(target_arch = "aarch64")]
fn cyclic_int32_phi_facts_remove_number_tag_tests_from_machine_loop() {
    for source in [
        "(function(n,z){let s=z;for(let i=z;i<n;i=i+1)s=s+i;return s})",
        "(function(n,z){let s=z;for(let i=z;i<n;i++)s=s+i;return s})",
        "(function(n,z){let s=z;for(let i=n;i>z;i--)s=s+i;return s})",
        "(function(n,z){let s=z;for(let i=z;i<n;i++)s=(s+(i&1023))|0;return s})",
    ] {
        let machine = int32_loop_code(source, true);
        assert!(!machine.is_empty());
        assert!(
            !machine.lines().collect::<Vec<_>>().windows(3).any(|lines| {
                lines[0].trim().starts_with("movz w")
                    && lines[0].ends_with(", #1")
                    && lines[1].trim().starts_with("uxtb w")
                    && lines[2].trim().starts_with("cbnz x")
            }),
            "proven numeric guards must not leave constant-true branches: {machine}"
        );
        assert!(
            machine
                .lines()
                .filter(|line| line.trim().starts_with("subs xzr,"))
                .count()
                <= 3,
            "only the two entry tags and the poll budget need 64-bit comparisons: {machine}"
        );
        assert!(
            !machine
                .lines()
                .any(|line| line.trim().starts_with("subs ") && line.ends_with(", #8")),
            "an Int32-only sum loop must not test for JS_TAG_FLOAT64: {machine}"
        );
        assert!(
            machine.contains("adds "),
            "checked integer arithmetic must remain: {machine}"
        );
    }
}

#[test]
fn counted_loop_increment_bounded_by_its_header_compare_needs_no_overflow_exit() {
    // `i < n` at the header and no other write to `i` prove `i + 1` fits, so
    // only `sum + i` keeps its overflow exit.
    let clif = int32_loop_clif("(function(n,z){let s=z;for(let i=z;i<n;i++){s=s+i}return s})");
    assert_eq!(
        clif.matches("sadd_overflow").count(),
        1,
        "only the accumulator add is checked: {clif}"
    );
}

#[test]
fn increments_without_a_strict_upper_bound_keep_their_overflow_exit() {
    // `<=` admits `i == INT32_MAX` before the increment; a second write to
    // the counter breaks the single-writer proof. Both keep the exact exit.
    for source in [
        "(function(n,z){let s=z;for(let i=z;i<=n;i++){s=s+i}return s})",
        "(function(n,z){let s=z;for(let i=z;i<n;i++){s=s+i;i=i+z}return s})",
    ] {
        let clif = int32_loop_clif(source);
        assert!(
            clif.matches("sadd_overflow").count() >= 2,
            "{source}: increment must stay checked: {clif}"
        );
    }
}

#[test]
fn signature_completes_when_the_first_invocation_returns_natively() {
    // With loop and call thresholds of one, a long first invocation enters
    // baseline code through OSR and returns natively, so the interpreter's
    // OP_return feedback never fires for it. Native DONE exits now record
    // the return type, and the numeric signature still completes; before,
    // Tier 2 admission waited forever for the missing return observation.
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
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
            ctx.eval::<(), _>(
                "function kernel(batches,seed){let checksum=seed;for(let batch=0;batch<batches;batch+=1){let a=0;let b=1;for(let i=0;i<40;i+=1){const next=a+b;a=b;b=next;}checksum=b;}return checksum;}",
            )
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 600 } else { 120 });
    let mut saw_tier2 = false;
    while std::time::Instant::now() < deadline {
        let result = context.with(|ctx| {
            let kernel: Function = ctx.globals().get("kernel").unwrap();
            kernel.call::<_, i32>((20_000, 0)).unwrap()
        });
        assert_eq!(result, 165_580_141);
        jit.poll();
        let metrics = jit.metrics();
        assert_eq!(metrics.native_fallbacks, 0, "{metrics:?}");
        if metrics.tier2_entries > 0 {
            saw_tier2 = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(saw_tier2, "{:?}", jit.metrics());
}

#[test]
fn bool_argument_compiles_a_tagged_tier2_artifact_without_direct_entry() {
    use rquickjs_jit::compiler::Compiler;
    let fixture = SnapshotFixture::compile(
        "(function incrementIf(value,enabled){let result=value+1;if(enabled)return result;return value;})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let mut feedback = FeedbackTable::new(32, 2);
    feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Bool]);
    for instruction in verified.instructions() {
        match instruction.opcode().name() {
            "add" => {
                feedback.observe_binary(
                    key,
                    instruction.pc(),
                    ObservedType::Int32,
                    ObservedType::Int32,
                    ObservedType::Int32,
                    Default::default(),
                );
            }
            "return" => {
                feedback.observe_return(key, instruction.pc(), ObservedType::Int32);
            }
            _ => {}
        }
    }
    let frozen = feedback.snapshot(77);
    assert!(frozen.bounded_specialization(key).is_some());
    let compiler = Tier2Compiler::host(77);
    assert!(
        compiler
            .lower_direct_call_with_feedback_for_test(&verified, key, &frozen)
            .is_err(),
        "local state remains outside the pure branch-leaf direct ABI"
    );
    let mut coordinator = Coordinator::with_limits(4, 4, 4, 1 << 20);
    coordinator
        .queue(key, Tier::Baseline, verified.clone())
        .unwrap();
    let baseline = coordinator.begin_next().unwrap();
    coordinator.complete(CompileCompletion {
        key,
        requested_tier: Tier::Baseline,
        artifact_key: baseline.artifact_key(),
        attempt_id: baseline.attempt_id(),
        result: Ok(CompiledArtifact::empty(baseline.artifact_key())),
    });
    coordinator
        .queue_with_feedback(key, Tier::Optimizing, verified, frozen)
        .unwrap();
    let artifact = compiler.compile(coordinator.begin_next().unwrap()).unwrap();
    assert!(
        artifact
            .optimized_metadata()
            .unwrap()
            .direct_call_signature()
            .is_none(),
        "failed secondary compilation must leave the ordinary optimized artifact intact"
    );
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn bool_argument_tagged_tier2_preserves_branches_and_type_change_deopt() {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function incrementIf(value,enabled){if(enabled)return value+1;return value;}",
            )
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 75 } else { 15 });
    while jit.metrics().tier2_entries == 0 {
        context.with(|ctx| {
            let function: Function = ctx.globals().get("incrementIf").unwrap();
            assert_eq!(function.call::<_, i32>((41, true)).unwrap(), 42);
            assert_eq!(function.call::<_, i32>((41, false)).unwrap(), 41);
        });
        jit.poll();
        assert!(std::time::Instant::now() < deadline, "{:?}", jit.metrics());
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let before = jit.metrics();
    context.with(|ctx| {
        let function: Function = ctx.globals().get("incrementIf").unwrap();
        assert_eq!(function.call::<_, i32>((41, true)).unwrap(), 42);
        assert_eq!(function.call::<_, i32>((41, false)).unwrap(), 41);
    });
    let after = jit.metrics();
    assert_eq!(after.tier2_entries - before.tier2_entries, 2);
    assert_eq!(after.deopts, before.deopts);
    context.with(|ctx| {
        let function: Function = ctx.globals().get("incrementIf").unwrap();
        // QuickJS Bool truthiness reads only the low 32 bits of the value
        // union. Exercise the newly specialized Bool entry with dirty padding,
        // not just the general tagged truthiness lowerer.
        for (payload, expected) in [(0x1234_5678_0000_0000, 41), (0x1234_5678_0000_0001, 42)] {
            let raw = rquickjs::qjs::JSValue {
                u: rquickjs::qjs::JSValueUnion {
                    float64: f64::from_bits(payload),
                },
                tag: i64::from(rquickjs::qjs::JS_TAG_BOOL),
            };
            // SAFETY: this is an immediate Bool with a canonical low-32-bit
            // payload; its unused upper bits own no allocation or reference.
            let enabled = unsafe { rquickjs::Value::from_raw(ctx.clone(), raw) };
            assert_eq!(function.call::<_, i32>((41, enabled)).unwrap(), expected);
        }
    });
    let padded = jit.metrics();
    assert_eq!(
        padded.tier2_entries - after.tier2_entries,
        2,
        "both padded Bool calls must execute the specialized Tier2 entry"
    );
    assert_eq!(padded.deopts, after.deopts);
    assert_eq!(padded.native_fallbacks, after.native_fallbacks);
    context.with(|ctx| {
        let function: Function = ctx.globals().get("incrementIf").unwrap();
        assert_eq!(function.call::<_, i32>((41, 0)).unwrap(), 41);
    });
    assert!(
        jit.metrics().deopts > after.deopts,
        "a numeric false value must fail the Bool tag guard, not alias Bool as Int32"
    );
    context.with(|ctx| {
        let function: Function = ctx.globals().get("incrementIf").unwrap();
        assert_eq!(function.call::<_, f64>((i32::MAX, true)).unwrap(), 2147483648.0);
        assert_eq!(ctx.eval::<String, _>(
            "[incrementIf(41,0),incrementIf(41,1),incrementIf(41,null),incrementIf(41,''),incrementIf(41,'yes'),incrementIf(41,{})].join(',')"
        ).unwrap(), "41,42,41,41,42,42");
    });
    assert!(jit.metrics().deopts > after.deopts);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn semantic_values_merge_frame_assignments_at_a_diamond() {
    use rquickjs_jit::ir::{FrameSlot, ScalarValue};
    let fixture =
        SnapshotFixture::compile("(function(flag,a,b){let x;if(flag)x=a+1;else x=b+2;return x*3})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 201).unwrap();
    let graph = ir.scalar_graph();
    assert!(
        graph.values().iter().any(|value| {
            let ScalarValue::Phi {
                slot: FrameSlot::Local(_),
                inputs,
                ..
            } = value
            else {
                return false;
            };
            inputs.len() == 2
                && inputs.iter().all(|input| {
                    matches!(
                        graph.values()[input.value.index()],
                        ScalarValue::Binary { .. }
                    )
                })
        }),
        "{:#?}",
        graph.values()
    );
}

#[test]
fn semantic_values_link_stack_joins_to_predecessor_results() {
    use rquickjs_jit::ir::{FrameSlot, ScalarValue};
    let fixture = SnapshotFixture::compile("(function(flag,a,b){return (flag ? a+1 : b+2)*3})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 202).unwrap();
    let graph = ir.scalar_graph();
    assert!(graph.values().iter().any(|value| matches!(value,
        ScalarValue::Phi { slot: FrameSlot::Stack(0), inputs, .. } if inputs.len() == 2
            && inputs.iter().all(|input| matches!(graph.values()[input.value.index()], ScalarValue::Binary { .. }))
    )), "{:#?}", graph.values());
}

#[test]
fn semantic_values_preserve_the_initial_entry_of_a_loop_at_pc_zero() {
    use rquickjs_jit::ir::{FrameSlot, ScalarValue};
    let fixture = SnapshotFixture::compile("(function(n){while(n>0)n=n-1;return n})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 203).unwrap();
    assert!(ir
        .blocks()
        .iter()
        .any(|block| block.start_pc() == 0 && block.is_loop_header()));
    let graph = ir.scalar_graph();
    assert!(graph.values().iter().any(|value| matches!(value,
        ScalarValue::Phi { block_pc: 0, slot: FrameSlot::Argument(0), inputs }
            if inputs.iter().any(|input| input.predecessor.is_none())
                && inputs.iter().any(|input| input.predecessor.is_some()
                    && matches!(graph.values()[input.value.index()], ScalarValue::Binary { .. }))
    )), "{:#?}", graph.values());
}

#[test]
fn semantic_values_capture_exact_pre_effect_stack_and_frame_assignments() {
    use rquickjs_jit::ir::ScalarValue;
    let fixture = SnapshotFixture::compile("(function(a,b){let x=a+1;return x*b})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 204).unwrap();
    let node = ir
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(),
        OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "mul")
        })
        .unwrap();
    let graph = ir.scalar_graph();
    let state = graph.frame_state_for_node(node.id()).unwrap();
    assert_eq!(state.pc, node.pc());
    assert_eq!(state.arguments.len(), 2);
    assert_eq!(state.stack.len(), 2);
    assert!(
        matches!(
            graph.values()[state.stack[0].index()],
            ScalarValue::Binary { .. }
        ),
        "{state:#?}\n{:#?}\n{:#?}",
        graph.values(),
        ir.nodes()
    );
    assert!(state.locals.contains(&state.stack[0]));
    assert_eq!(state.stack[1], state.arguments[1]);
    assert_eq!(
        graph.binary_operands(node.id()),
        Some((state.stack[0], state.stack[1]))
    );
}

#[test]
fn semantic_frame_states_distinguish_entry_assignment_and_call_effects() {
    use rquickjs_jit::ir::{FrameSlot, ScalarValue};
    for (source, check) in [
        ("(function(n){while(n>0)n=n-1;return n})", 0),
        ("(function(a,b){return (a=b)+a})", 1),
        ("(function(a,f){return a+f()})", 2),
    ] {
        let fixture = SnapshotFixture::compile(source);
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&verified, 205).unwrap();
        let graph = ir.scalar_graph();
        if check == 0 {
            let state = graph.frame_state_for_node(ir.nodes()[0].id()).unwrap();
            assert!(matches!(
                graph.values()[state.arguments[0].index()],
                ScalarValue::Input {
                    block_pc: 0,
                    slot: FrameSlot::Argument(0)
                }
            ));
            assert!(state.stack.is_empty());
            continue;
        }
        let add = ir
            .nodes()
            .iter()
            .find(|node| {
                matches!(node.kind(),
            OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "add")
            })
            .unwrap();
        let state = graph.frame_state_for_node(add.id()).unwrap();
        assert_eq!(state.stack.len(), 2);
        if check == 1 {
            assert_eq!(state.arguments[0], state.arguments[1]);
            assert_eq!(state.stack.as_ref(), &[state.arguments[1]; 2]);
        } else {
            // A value already on the operand stack survives a call, while
            // frame slots are reloaded after its possible reentrant effects.
            assert!(matches!(
                graph.values()[state.stack[0].index()],
                ScalarValue::Input {
                    slot: FrameSlot::Argument(0),
                    ..
                }
            ));
            assert!(matches!(
                graph.values()[state.arguments[0].index()],
                ScalarValue::FrameRead {
                    slot: FrameSlot::Argument(0),
                    ..
                }
            ));
            assert_ne!(state.stack[0], state.arguments[0]);
        }
    }
}

#[test]
fn semantic_calls_have_explicit_operands_and_recovery_state() {
    use rquickjs_jit::ir::ScalarValue;
    for (source, argc, method) in [
        ("(function(f){return f()})", 0, false),
        ("(function(f,a){return f(a)})", 1, false),
        ("(function(f,a,b){return f(a,b)})", 2, false),
        ("(function(f,a,b,c){return f(a,b,c)})", 3, false),
        ("(function(f,a,b,c,d){return f(a,b,c,d)})", 4, false),
        ("(function(o,a,b){return o.method(a,b)})", 2, true),
    ] {
        let fixture = SnapshotFixture::compile(source);
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&verified, 205).unwrap();
        let graph = ir.scalar_graph();
        let call = ir
            .nodes()
            .iter()
            .find(|node| {
                matches!(node.kind(),
            OptimizedNodeKind::Bytecode { opcode } if opcode.starts_with("call"))
            })
            .unwrap();
        let result = graph.value_for_node(call.id()).unwrap();
        assert!(
            !matches!(graph.values()[result.index()], ScalarValue::Opaque),
            "call operands must be modeled in SSA: {source}"
        );
        let state = graph.frame_state_for_node(call.id()).unwrap();
        let operands = graph.call(call.id()).unwrap();
        assert_eq!(operands.frame_state_node, call.id());
        assert_eq!(operands.arguments.len(), argc);
        assert_eq!(operands.receiver.is_some(), method);
        assert_eq!(state.stack.len(), argc + 1 + usize::from(method));
        assert_eq!(state.stack[usize::from(method)], operands.target);
        assert_eq!(
            &state.stack[1 + usize::from(method)..],
            operands.arguments.as_ref()
        );
        assert_eq!(operands.arguments.as_ref(), &state.arguments[1..]);
        if method {
            assert_eq!(operands.receiver, Some(state.stack[0]));
        } else {
            assert_eq!(operands.target, state.arguments[0]);
        }
        assert!(
            !state.stack.contains(&result),
            "a call produces a fresh value"
        );
    }
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn semantic_frame_state_deopt_preserves_phi_and_parameter_assignment() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function target(a,b,flag){let x;if(flag)x=a;else x=b;return x*(a=b)+a}",
            )
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("target(2,3,1)"))
                .unwrap(),
            9
        );
        jit.poll();
    }
    let before = jit.metrics();
    assert!(before.tier2_entries >= 10, "{before:?}");
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("target(2147483647,2,1)"))
            .unwrap(),
        4294967296.0
    );
    let overflow = jit.metrics();
    // Argument tags still match the warm calls. Overflow must exit from
    // arithmetic after the Phi and assignment, not an entry type guard.
    assert!(
        overflow.tier2_entries > before.tier2_entries,
        "{overflow:?}"
    );
    assert!(overflow.deopts > before.deopts, "{overflow:?}");
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<String, _>(
                "let hits=0;let value={valueOf(){hits++;return 3}};target(7,value,1)+':'+hits"
            ))
            .unwrap(),
        "24:2"
    );
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert!(after.deopts > before.deopts, "{after:?}");
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux"),
    target_endian = "little",
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
#[test]
fn semantic_values_execute_both_phi_edges_and_loop_entry_natively() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    for (source, warm, expected_warm, probe, expected_probe) in [
        (
            "function target(flag,a,b){let x;if(flag)x=a+1;else x=b+2;return x*3}",
            "target(1,2,4)",
            9,
            "target(0,2,4)",
            18,
        ),
        (
            "function target(flag,a,b){return (flag?a+1:b+2)*3}",
            "target(1,2,4)",
            9,
            "target(0,2,4)",
            18,
        ),
        (
            "function target(n){while(n>0)n=n-1;return n}",
            "target(10)",
            0,
            "target(-7)",
            -7,
        ),
        (
            "function target(n,a,b){while(n>0){let t=a;a=b;b=t;n--}return a*10+b}",
            "target(4,2,7)",
            27,
            "target(3,2,7)",
            72,
        ),
        (
            "function target(a,b){return (a=b)+a}",
            "target(2,7)",
            14,
            "target(9,3)",
            6,
        ),
        (
            "function target(a,b){let x=a;return x++ + x*b}",
            "target(2,7)",
            23,
            "target(9,3)",
            39,
        ),
        (
            "function target(a,b){let x;return (x=(a=b))+x+a}",
            "target(2,7)",
            21,
            "target(9,3)",
            9,
        ),
    ] {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .call_threshold(2)
                .force_optimized_for_test(true)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
        while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
            assert_eq!(
                context.with(|ctx| ctx.eval::<i32, _>(warm)).unwrap(),
                expected_warm
            );
            jit.poll();
        }
        let before = jit.metrics();
        assert!(before.tier2_entries >= 10, "{source}: {before:?}");
        assert_eq!(
            context.with(|ctx| ctx.eval::<i32, _>(probe)).unwrap(),
            expected_probe
        );
        let after = jit.metrics();
        assert!(
            after.tier2_entries > before.tier2_entries,
            "{source}: {after:?}"
        );
        assert_eq!(after.deopts, before.deopts, "{source}: {after:?}");
    }
}

#[test]
fn semantic_numeric_modes_are_selected_before_value_numbering() {
    use rquickjs_jit::ir::{ScalarBinaryOp, ScalarNumericMode, ScalarValue};
    let fixture = SnapshotFixture::compile("(function(a,b){return (a*b)+(a*b)+(a*b)})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let pcs = verified
        .instructions()
        .iter()
        .filter(|instruction| instruction.opcode().name() == "mul")
        .map(|instruction| instruction.pc())
        .collect::<Vec<_>>();
    assert_eq!(pcs.len(), 3);
    let modes = [
        ScalarNumericMode::Int32,
        ScalarNumericMode::Float64,
        ScalarNumericMode::Number,
    ];
    let feedback = pcs.iter().copied().zip(modes).collect();
    let ir = OptimizedIr::translate_with_numeric_modes(&verified, 206, &feedback).unwrap();
    let products = ir
        .scalar_graph()
        .values()
        .iter()
        .filter_map(|value| match value {
            ScalarValue::Binary {
                op: ScalarBinaryOp::Mul,
                mode,
                lhs,
                rhs,
            } => Some((*mode, *lhs, *rhs)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        products.len(),
        3,
        "distinct checked contracts cannot share a producer"
    );
    assert_eq!(
        products.iter().map(|product| product.0).collect::<Vec<_>>(),
        modes
    );
    for product in &products[1..] {
        assert_eq!((product.1, product.2), (products[0].1, products[0].2));
    }
    for (pc, mode) in pcs.into_iter().zip(modes) {
        let node = ir.nodes().iter().find(|node| node.pc() == pc).unwrap();
        assert_eq!(
            ir.scalar_graph().binary_operation(node.id()),
            Some((ScalarBinaryOp::Mul, mode))
        );
    }
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn semantic_float_mode_checks_a_phi_with_an_unobserved_integer_edge() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>("function target(flag,a,b){let x;if(flag)x=a;else x=1;return x+b}")
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<f64, _>("target(1,1.25,2.5)"))
                .unwrap(),
            3.75
        );
        jit.poll();
    }
    let before = jit.metrics();
    assert!(before.tier2_entries >= 10, "{before:?}");
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("target(0,1.25,2.5)"))
            .unwrap(),
        3.5
    );
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert!(after.deopts > before.deopts, "{after:?}");
}

#[test]
fn semantic_checks_eliminate_repeated_value_checks_but_not_frame_reloads() {
    use rquickjs_jit::ir::ScalarNumericMode;
    for mode in [
        ScalarNumericMode::Number,
        ScalarNumericMode::Int32,
        ScalarNumericMode::Float64,
    ] {
        let fixture = SnapshotFixture::compile("(function(a,b){return (a+b)*(a-b)})");
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let modes = verified
            .instructions()
            .iter()
            .map(|instruction| (instruction.pc(), mode))
            .collect();
        let ir = OptimizedIr::translate_with_numeric_modes(&verified, 207, &modes).unwrap();
        let checks = ir
            .nodes()
            .iter()
            .flat_map(|node| ir.scalar_graph().checks_for_node(node.id()))
            .collect::<Vec<_>>();
        assert_eq!(checks.len(), 6);
        assert_eq!(checks.iter().filter(|check| !check.eliminated).count(), 2);
        assert_eq!(ir.scalar_graph().eliminated_type_checks(), 4);
        for check in checks {
            assert!(ir
                .scalar_graph()
                .frame_state_for_node(check.frame_state_node)
                .is_some());
            assert_eq!(check.mode, mode);
        }
    }
    let fixture = SnapshotFixture::compile("(function(a,f){let x=a+1;f();return a*2+x})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 208).unwrap();
    let mul = ir.nodes().iter().find(|node| matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "mul")).unwrap();
    let checks = ir.scalar_graph().checks_for_node(mul.id());
    assert_eq!(checks.len(), 2);
    assert!(
        !checks[0].eliminated,
        "the call's frame reload requires a fresh check"
    );
    assert!(checks[1].eliminated, "the constant 2 is already Int32");
}

#[test]
fn semantic_checks_keep_stronger_contracts_and_unproven_cfg_inputs() {
    use rquickjs_jit::ir::ScalarNumericMode;
    let fixture = SnapshotFixture::compile("(function(a,b){return (a+b)*(a-b)})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let sub_pc = verified
        .instructions()
        .iter()
        .find(|instruction| instruction.opcode().name() == "sub")
        .unwrap()
        .pc();
    let modes = [(sub_pc, ScalarNumericMode::Float64)].into_iter().collect();
    let ir = OptimizedIr::translate_with_numeric_modes(&verified, 209, &modes).unwrap();
    let sub = ir.nodes().iter().find(|node| node.pc() == sub_pc).unwrap();
    assert!(
        ir.scalar_graph()
            .checks_for_node(sub.id())
            .iter()
            .all(|check| !check.eliminated),
        "Number observations do not prove Float64 payloads"
    );

    let fixture = SnapshotFixture::compile(
        "(function(flag,a,b){let x;if(flag)x=a+b;else x=a-b;return a*b+x})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 210).unwrap();
    let mul = ir.nodes().iter().find(|node| matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "mul")).unwrap();
    let checks = ir.scalar_graph().checks_for_node(mul.id());
    assert_eq!(checks.len(), 2);
    assert!(
        checks.iter().all(|check| check.eliminated),
        "both predecessor exits prove Number for each argument"
    );
}

#[test]
fn semantic_cfg_facts_require_all_edges_and_preserve_initial_loop_inputs() {
    for source in [
        "(function(flag,a,b){let x;if(flag)x=a+b;else x=1;return a*b+x})",
        "(function(flag,a,b,f){let x;if(flag){x=a+b;f()}else x=a-b;return a*b+x})",
    ] {
        let fixture = SnapshotFixture::compile(source);
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&verified, 211).unwrap();
        let mul = ir.nodes().iter().find(|node| matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "mul")).unwrap();
        let checks = ir.scalar_graph().checks_for_node(mul.id());
        assert_eq!(checks.len(), 2);
        assert!(
            checks.iter().all(|check| !check.eliminated),
            "{source}: {checks:?}"
        );
    }
    let fixture = SnapshotFixture::compile("(function(n){while(n>0)n=n-1;return n})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 212).unwrap();
    let sub = ir.nodes().iter().find(|node| matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "sub")).unwrap();
    let checks = ir.scalar_graph().checks_for_node(sub.id());
    assert!(
        checks.iter().all(|check| check.eliminated),
        "the successful loop comparison already proves Number operands"
    );
    let compare = ir
        .nodes()
        .iter()
        .find(|node| ir.scalar_graph().comparison(node.id()).is_some())
        .unwrap();
    let checks = ir.scalar_graph().checks_for_node(compare.id());
    assert!(
        !checks[0].eliminated,
        "the loop comparison must check the initial input"
    );
    assert!(checks[1].eliminated);
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn semantic_cfg_facts_deopt_on_an_unchecked_float_phi_edge() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>("function target(flag,a,b){let x;if(flag)x=a+1;else x=b;return x*2}")
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("target(1,2,3.5)"))
                .unwrap(),
            6
        );
        jit.poll();
    }
    let before = jit.metrics();
    assert!(before.tier2_entries >= 10, "{before:?}");
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("target(0,2,3.5)"))
            .unwrap(),
        7.0
    );
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert!(after.deopts > before.deopts, "{after:?}");
}

#[test]
fn semantic_cfg_fact_budget_restores_block_local_checks() {
    let fixture = SnapshotFixture::compile(
        "(function(flag,a,b){let x;if(flag)x=a+b;else x=a-b;return a*b+x})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let mut ir = OptimizedIr::translate(&verified, 213).unwrap();
    let mul = ir.nodes().iter().find(|node| matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "mul")).unwrap().id();
    assert!(ir
        .scalar_graph()
        .checks_for_node(mul)
        .iter()
        .all(|check| check.eliminated));
    ir.recompute_numeric_checks_for_test(1).unwrap();
    let checks = ir.scalar_graph().checks_for_node(mul);
    assert_eq!(checks.len(), 2);
    assert!(
        checks.iter().all(|check| !check.eliminated),
        "fallback must undo existing cross-block eliminations"
    );
    ir.recompute_numeric_checks_for_test(usize::MAX).unwrap();
    assert!(ir
        .scalar_graph()
        .checks_for_node(mul)
        .iter()
        .all(|check| check.eliminated));
}

#[test]
fn semantic_comparisons_use_explicit_operands_without_numeric_bool_facts() {
    use rquickjs_jit::ir::{ScalarCompareOp, ScalarValue};
    for (operator, expected) in [
        ("<", ScalarCompareOp::LessThan),
        ("<=", ScalarCompareOp::LessEqual),
        (">", ScalarCompareOp::GreaterThan),
        (">=", ScalarCompareOp::GreaterEqual),
    ] {
        let fixture = SnapshotFixture::compile(&format!(
            "(function(a,b,c,d){{return (a+b){operator}(c+d)}})"
        ));
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&verified, 214).unwrap();
        let graph = ir.scalar_graph();
        let node = ir
            .nodes()
            .iter()
            .find(|node| graph.comparison(node.id()).is_some())
            .unwrap();
        let (op, lhs, rhs) = graph.comparison(node.id()).unwrap();
        assert_eq!(op, expected);
        assert!(matches!(
            graph.values()[lhs.index()],
            ScalarValue::Binary { .. }
        ));
        assert!(matches!(
            graph.values()[rhs.index()],
            ScalarValue::Binary { .. }
        ));
        assert_eq!(
            graph
                .frame_state_for_node(node.id())
                .unwrap()
                .stack
                .as_ref(),
            &[lhs, rhs]
        );
        let checks = graph.checks_for_node(node.id());
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|check| check.eliminated));
    }
    let fixture = SnapshotFixture::compile("(function(a,b){return (a<b)+1})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 215).unwrap();
    let add = ir.nodes().iter().find(|node| matches!(node.kind(), OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "add")).unwrap();
    assert!(
        !ir.scalar_graph().checks_for_node(add.id())[0].eliminated,
        "a comparison produces Bool, which requires arithmetic coercion"
    );
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn semantic_comparisons_preserve_nan_and_negative_zero_natively() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    for (operator, warm, zero) in [
        ("<", true, false),
        ("<=", true, true),
        (">", false, false),
        (">=", false, true),
    ] {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .call_threshold(2)
                .force_optimized_for_test(true)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| ctx.eval::<(), _>(format!("function target(a,b){{return a{operator}b}}")))
            .unwrap();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
        while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
            assert_eq!(
                context
                    .with(|ctx| ctx.eval::<bool, _>("target(1.25,2.5)"))
                    .unwrap(),
                warm
            );
            jit.poll();
        }
        let before = jit.metrics();
        assert!(before.tier2_entries >= 10, "{operator}: {before:?}");
        for (probe, expected) in [
            ("target(NaN,2.5)", false),
            ("target(1.25,NaN)", false),
            ("target(-0,-0)", zero),
        ] {
            assert_eq!(
                context.with(|ctx| ctx.eval::<bool, _>(probe)).unwrap(),
                expected,
                "{operator}: {probe}"
            );
        }
        let after = jit.metrics();
        assert_eq!(
            after.tier2_entries - before.tier2_entries,
            3,
            "{operator}: {after:?}"
        );
        assert_eq!(after.deopts, before.deopts, "{operator}: {after:?}");
    }
}

#[test]
fn semantic_updates_link_postfix_outputs_and_local_writes() {
    use rquickjs_jit::ir::{ScalarNumericMode, ScalarValue};
    let mut seen = std::collections::BTreeSet::new();
    let mut snapshots = [
        "(function(x){return x++ + x})",
        "(function(x){return x-- + x})",
        "(function(x){return ++x})",
        "(function(x){return --x})",
        "(function(x){var y=x;y++;return y})",
        "(function(x){var y=x;y--;return y})",
    ]
    .into_iter()
    .map(|source| SnapshotFixture::compile(source).snapshot().clone())
    .collect::<Vec<_>>();
    let opcode = |name| {
        rquickjs_jit::bytecode::linked_opcode_table()
            .find(|opcode| opcode.name() == name)
            .unwrap()
            .id()
    };
    for name in ["inc_loc", "dec_loc"] {
        snapshots.push(CompileSnapshot::from_untrusted_bytecode(
            vec![
                opcode("get_arg0"),
                opcode("put_loc0"),
                opcode(name),
                0,
                opcode("get_loc0"),
                opcode("return"),
            ],
            1,
            1,
            0,
            0,
        ));
    }
    for snapshot in snapshots {
        let verified = snapshot.verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&verified, 300).unwrap();
        let graph = ir.scalar_graph();
        for node in ir.nodes() {
            let rquickjs_jit::ir::OptimizedNodeKind::Bytecode { opcode } = node.kind() else {
                continue;
            };
            if !matches!(
                opcode.as_ref(),
                "inc" | "dec" | "post_inc" | "post_dec" | "inc_loc" | "dec_loc"
            ) {
                continue;
            }
            let Some((mode, input, delta)) = graph.update_operation(node.id()) else {
                panic!("update bytecode has no semantic update: {opcode}");
            };
            assert_eq!(mode, ScalarNumericMode::Number);
            seen.insert(opcode.to_string());
            assert_eq!(delta, if opcode.contains("inc") { 1 } else { -1 });
            let result = graph.value_for_node(node.id()).unwrap();
            assert!(matches!(
                graph.values()[result.index()],
                ScalarValue::Update { .. }
            ));
            assert_eq!(graph.checks_for_node(node.id()).len(), 1);
            assert_eq!(graph.checks_for_node(node.id())[0].value, input);
            assert!(graph.frame_state_for_node(node.id()).is_some());
            if opcode.starts_with("post_") {
                assert_eq!(graph.outputs_for_node(node.id()), [input, result]);
            } else if opcode.ends_with("_loc") {
                assert!(graph.outputs_for_node(node.id()).is_empty());
                assert_eq!(graph.frame_definitions_for_node(node.id())[0].1, result);
            } else {
                assert_eq!(graph.outputs_for_node(node.id()), [result]);
            }
        }
    }
    assert_eq!(
        seen,
        ["inc", "dec", "post_inc", "post_dec", "inc_loc", "dec_loc"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn semantic_postfix_overflow_restores_pre_update_state() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "function target(n,x){let last=x;for(let i=0;i<n;i++){last=x++;}return last+x}",
            )
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("target(2,3)"))
                .unwrap(),
            9
        );
        jit.poll();
    }
    let before = jit.metrics();
    assert!(before.tier2_entries >= 10, "{before:?}");
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("target(1,2147483647)"))
            .unwrap(),
        4294967295.0
    );
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert!(after.deopts > before.deopts, "{after:?}");
    assert_eq!(after.native_entries, after.native_exits);
}

#[test]
fn caller_request_retains_versioned_callee_snapshot_and_budget_after_retirement() {
    use rquickjs_jit::compiler::{baseline::BaselineCompiler, Compiler};
    for constrained in [false, true] {
        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        let ((callee_snapshot, callee_constants), (caller_snapshot, caller_constants)) = context
            .with(|ctx| {
                let callee: rquickjs::Value = ctx.eval("(function(a){return a+1+2+3+4+5+6+7+8+9+10+11+12+13+14+15+16+17+18+19+20})").unwrap();
                let caller: rquickjs::Value = ctx.eval("(function(f,a){return f(a)})").unwrap();
                unsafe {
                    (
                        CompileSnapshot::capture_with_runtime_constants(
                            &runtime,
                            ctx.as_raw().as_ptr(),
                            callee.as_raw(),
                        )
                        .unwrap(),
                        CompileSnapshot::capture_with_runtime_constants(
                            &runtime,
                            ctx.as_raw().as_ptr(),
                            caller.as_raw(),
                        )
                        .unwrap(),
                    )
                }
            });
        let callee_body = callee_snapshot.verify(VerifyLimits::default()).unwrap();
        let caller_body = caller_snapshot.verify(VerifyLimits::default()).unwrap();
        let callee = FunctionKey::new(
            callee_body.snapshot().function_id(),
            callee_body.snapshot().generation(),
        );
        let caller = FunctionKey::new(
            caller_body.snapshot().function_id(),
            caller_body.snapshot().generation(),
        );
        assert_ne!(caller, callee);
        let return_pc = callee_body
            .instructions()
            .iter()
            .find(|i| i.opcode().name() == "return")
            .unwrap()
            .pc();
        let call_pc = caller_body
            .instructions()
            .iter()
            .find(|i| {
                i.opcode().name().starts_with("call") || i.opcode().name().starts_with("tail_call")
            })
            .unwrap()
            .pc();
        let mut feedback = FeedbackTable::new(64, 2);
        for _ in 0..32 {
            feedback.observe_call(
                caller,
                &[ObservedType::Function(callee), ObservedType::Int32],
            );
            feedback.observe_call(callee, &[ObservedType::Int32]);
            feedback.observe_return(callee, return_pc, ObservedType::Int32);
            for instruction in callee_body
                .instructions()
                .iter()
                .filter(|i| i.opcode().name() == "add")
            {
                feedback.observe_binary(
                    callee,
                    instruction.pc(),
                    ObservedType::Int32,
                    ObservedType::Int32,
                    ObservedType::Int32,
                    Default::default(),
                );
            }

            feedback.observe_call_signature_with_identity(
                caller,
                call_pc,
                callee,
                0x12345678,
                0x22345678,
                &[ObservedType::Int32],
                ObservedType::Int32,
            );
        }
        assert!(
            feedback
                .snapshot(301)
                .bounded_specialization(callee)
                .is_some(),
            "missing feedback signature"
        );
        Tier2Compiler::host(301)
            .lower_direct_call_with_feedback_for_test(&callee_body, callee, &feedback.snapshot(301))
            .unwrap_or_else(|error| {
                panic!(
                    "direct leaf: {error:?}; opcodes {:?}",
                    callee_body
                        .instructions()
                        .iter()
                        .map(|i| i.opcode().name())
                        .collect::<Vec<_>>()
                )
            });
        let mut coordinator = Coordinator::with_limits(4, 4, 4, 1 << 20);
        coordinator
            .queue_with_feedback(
                callee,
                Tier::Baseline,
                callee_body.clone(),
                feedback.snapshot(301),
            )
            .unwrap();
        let request = coordinator.begin_next().unwrap();
        let key = request.artifact_key();
        let attempt = request.attempt_id();
        let artifact = Compiler::compile(&BaselineCompiler::host(), request).unwrap();
        assert!(artifact.inline_snapshot().is_some());
        assert!(
            artifact
                .optimized_metadata()
                .and_then(|m| m.direct_call_signature())
                .is_some(),
            "no direct signature"
        );
        coordinator.complete(CompileCompletion {
            key: callee,
            requested_tier: Tier::Baseline,
            artifact_key: key,
            attempt_id: attempt,
            result: Ok(artifact),
        });
        assert_eq!(
            coordinator.state(callee),
            rquickjs_jit::runtime::CompileState::Installed(Tier::Baseline)
        );
        let caller_bytes = caller_body.snapshot().owned_bytes();
        let callee_bytes = callee_body.snapshot().retained_bytes();
        coordinator
            .queue_with_feedback(caller, Tier::Baseline, caller_body, feedback.snapshot(301))
            .unwrap();
        if constrained {
            use rquickjs_jit::{compiler::mock::FakeCompiler, runtime::BackgroundCompiler};
            let (compiler, control) = FakeCompiler::new(1);
            let mut workers = BackgroundCompiler::new_with_resource_limits(
                std::sync::Arc::new(compiler),
                1,
                1,
                std::time::Duration::from_secs(30),
                caller_bytes,
                caller_bytes * 32,
            )
            .unwrap();
            assert!(
                workers.dispatch_next(&mut coordinator).unwrap(),
                "optional callee retention must not reject an otherwise affordable compilation"
            );
            let request = control
                .request_within(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(request.snapshot_bytes(), caller_bytes);
            let target = request.direct_call_target(call_pc).unwrap();
            assert!(target.inline_snapshot().is_none());
            assert!(!target.entry().is_null());
            assert_eq!(workers.live_usage(), (1, caller_bytes, caller_bytes * 32));
            control.complete(CompiledArtifact::fake(Tier::Baseline));
            workers.shutdown(&mut coordinator);
            assert_eq!(workers.live_usage(), (0, 0, 0));
            continue;
        }
        let request = coordinator.begin_next().unwrap();
        assert_eq!(request.snapshot_bytes(), caller_bytes + callee_bytes);
        let target = request.direct_call_target(call_pc).unwrap();
        assert_eq!(target.artifact_key(), key);
        let retained = target.inline_snapshot().unwrap().clone();
        assert_eq!(retained.bytecode(), callee_body.snapshot().bytecode());
        let caller_body = request.snapshot().clone();
        let caller_artifact =
            Compiler::compile(&BaselineCompiler::host(), request.clone()).unwrap();
        coordinator.complete(CompileCompletion {
            key: caller,
            requested_tier: Tier::Baseline,
            artifact_key: request.artifact_key(),
            attempt_id: request.attempt_id(),
            result: Ok(caller_artifact),
        });
        coordinator
            .queue_with_feedback(
                caller,
                Tier::Optimizing,
                caller_body,
                feedback.snapshot(301),
            )
            .unwrap();
        let optimizing = coordinator.begin_next().unwrap();
        let artifact = Compiler::compile(&Tier2Compiler::host(301), optimizing.clone()).unwrap();
        assert_eq!(
            artifact.optimized_metadata().unwrap().inlined_calls(),
            1,
            "production compilation must consume retained callee bytecode"
        );
        // Optional inlining must not prevent compilation when only the
        // ordinary caller graph fits the worker's retained-IR budget.
        let compile_with_limit = |limit| {
            let control = rquickjs_jit::compiler::CompileControl::with_ir_limit(
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                std::time::Duration::from_secs(30),
                limit,
            );
            Compiler::compile_controlled(&Tier2Compiler::host(301), optimizing.clone(), &control)
        };
        let (mut low, mut high) = (0, 1 << 20);
        while low < high {
            let middle = low + (high - low) / 2;
            match compile_with_limit(middle) {
                Ok(_) => high = middle,
                Err(rquickjs_jit::compiler::CompileFailure::ResourceLimit) => low = middle + 1,
                Err(error) => panic!("unexpected controlled compilation failure: {error:?}"),
            }
        }
        let affordable = compile_with_limit(low).unwrap();
        assert_eq!(
            affordable.optimized_metadata().unwrap().inlined_calls(),
            0,
            "the smallest affordable compilation must retain ordinary calls"
        );
        coordinator.retire(callee);
        drop(coordinator);
        drop(callee_body);
        drop(callee_constants);
        drop(caller_constants);
        drop(context);
        drop(runtime);
        assert!(retained.verify(VerifyLimits::default()).is_ok());
        assert_eq!(
            request
                .direct_call_target(call_pc)
                .unwrap()
                .inline_snapshot()
                .unwrap()
                .bytecode(),
            retained.bytecode()
        );
    }
}

#[test]
fn frame_inline_call2_result_can_be_duplicated() {
    use rquickjs_jit::ir::FrameInlineCallee;
    let caller_fixture = SnapshotFixture::compile("(function(f,o,n){return f(o,n)||n})");
    let callee_fixture = SnapshotFixture::compile("(function(o,n){o.value=n;return n})");
    let caller_body = caller_fixture
        .snapshot()
        .verify(VerifyLimits::default())
        .unwrap();
    let body = callee_fixture
        .snapshot()
        .verify(VerifyLimits::default())
        .unwrap();
    let caller = FunctionKey::new(
        caller_body.snapshot().function_id(),
        caller_body.snapshot().generation(),
    );
    let callee = FunctionKey::new(body.snapshot().function_id(), body.snapshot().generation());
    let pair = caller_body
        .instructions()
        .windows(2)
        .find(|pair| pair[0].opcode().name() == "call2" && pair[1].opcode().name() == "dup")
        .expect("fixture must exercise call2 followed immediately by dup");
    let call_pc = pair[0].pc();
    let mut feedback = FeedbackTable::new(64, 2);
    for _ in 0..32 {
        feedback.observe_call(
            caller,
            &[
                ObservedType::Function(callee),
                ObservedType::Object,
                ObservedType::Int32,
            ],
        );
        feedback.observe_call_signature_with_identity(
            caller,
            call_pc,
            callee,
            0x12345678,
            0x22345678,
            &[ObservedType::Object, ObservedType::Int32],
            ObservedType::Int32,
        );
    }
    let feedback = feedback.snapshot(412);
    let mut artifact = CompiledArtifact::fake(Tier::Baseline).key();
    artifact.function_id = callee.id;
    artifact.generation = callee.generation;
    artifact.source_revision = body.snapshot().source_revision();
    artifact.opcode_fingerprint = body.snapshot().opcode_fingerprint();
    let candidate = FrameInlineCallee {
        artifact,
        target: feedback.call_link_at(caller, call_pc).unwrap(),
        body,
        children: Default::default(),
    };
    let (ir, _) = Tier2Compiler::host(412)
        .lower_with_frame_inline_callee_for_test(
            &caller_body,
            caller,
            &feedback,
            call_pc,
            candidate,
        )
        .expect("owned frame-inline result duplication must compile on its first attempt");
    assert_eq!(ir.scalar_graph().frame_inlined_calls(), 1);
}

#[test]
fn effect_free_inline_regions_bind_caller_values_and_fold_known_branches() {
    use rquickjs_jit::ir::{InlineCallee, ScalarValue};
    for (source, callee_source, representations, expected) in [
        (
            "(function(f,a,b){return f(a,b)})",
            "(function(a,b){return a+b})",
            vec![ObservedType::Int32, ObservedType::Int32],
            1,
        ),
        (
            "(function(f,a){return f(a,true)})",
            "(function(a,enabled){if(enabled)return a+1;return a})",
            vec![ObservedType::Int32, ObservedType::Bool],
            1,
        ),
        (
            "(function(f,a){let value=a;for(let i=0;i<1000;i++){value=f(value,true)}return value})",
            "(function(a,enabled){if(enabled)return a+1;return a})",
            vec![ObservedType::Int32, ObservedType::Bool],
            1,
        ),
        (
            "(function(f,a,b){return f(a,b)})",
            "(function(a,enabled){if(enabled)return a+1;return a})",
            vec![ObservedType::Int32, ObservedType::Bool],
            0,
        ),
        (
            "(function(f,a,b){return f(a,b)})",
            "(function(a,b){globalThis.hits++;return a+b})",
            vec![ObservedType::Int32, ObservedType::Int32],
            0,
        ),
    ] {
        let caller_fixture = SnapshotFixture::compile(source);
        let caller_body = caller_fixture
            .snapshot()
            .verify(VerifyLimits::default())
            .unwrap();
        let callee_fixture = SnapshotFixture::compile(callee_source);
        let body = callee_fixture
            .snapshot()
            .verify(VerifyLimits::default())
            .unwrap();
        let caller = FunctionKey::new(
            caller_body.snapshot().function_id(),
            caller_body.snapshot().generation(),
        );
        let callee = FunctionKey::new(body.snapshot().function_id(), body.snapshot().generation());
        let call_pc = caller_body
            .instructions()
            .iter()
            .find(|i| i.opcode().name().contains("call"))
            .unwrap()
            .pc();
        let mut feedback = FeedbackTable::new(64, 2);
        let mut caller_types = vec![ObservedType::Function(callee)];
        caller_types.extend(
            representations
                .iter()
                .copied()
                .take(usize::from(caller_body.snapshot().arg_count()) - 1),
        );
        for _ in 0..32 {
            feedback.observe_call(caller, &caller_types);
            feedback.observe_call_signature_with_identity(
                caller,
                call_pc,
                callee,
                0x12345678,
                0x22345678,
                &representations,
                ObservedType::Int32,
            );
        }
        let feedback = feedback.snapshot(302);
        let mut artifact = CompiledArtifact::fake(Tier::Baseline).key();
        artifact.function_id = callee.id;
        artifact.generation = callee.generation;
        artifact.source_revision = body.snapshot().source_revision();
        artifact.opcode_fingerprint = body.snapshot().opcode_fingerprint();
        let candidate = InlineCallee {
            artifact,
            call: feedback.call_specialization_at(caller, call_pc).unwrap(),
            body,
        };
        let (ir, clif) = Tier2Compiler::host(302)
            .lower_with_inline_callee_for_test(&caller_body, caller, &feedback, call_pc, candidate)
            .unwrap();
        let graph = ir.scalar_graph();
        assert_eq!(
            graph.inlined_calls(),
            expected,
            "{source} -> {callee_source}"
        );
        let node = ir
            .nodes()
            .iter()
            .find(|node| graph.call(node.id()).is_some())
            .unwrap();
        let call = graph.call(node.id()).unwrap();
        if expected == 0 {
            assert_eq!(node.effect(), OptimizedEffect::Reentrant);
            assert!(call.inline.is_none());
            continue;
        }
        assert_eq!(node.effect(), OptimizedEffect::Control);
        let region = call.inline.as_ref().unwrap();
        assert_eq!(region.artifact, artifact);
        let ScalarValue::Binary { lhs, .. } = graph.values()[region.result.index()] else {
            panic!("missing inline add");
        };
        assert_eq!(lhs, call.arguments[0]);
        for step in region.steps.iter().filter(|step| step.frame.is_some()) {
            let frame = step.frame.as_ref().unwrap();
            assert_eq!(frame.parent, Some(node.id()));
            assert_eq!(frame.arguments, call.arguments);
        }
        assert!(clif.contains("sadd_overflow"), "{clif}");
        // Recovery may call ownership helpers. Walk every non-cold path from
        // entry rather than mistaking cold recovery blocks for the hot region.
        let mut blocks = std::collections::BTreeMap::<u32, (bool, Vec<&str>)>::new();
        let mut current = None;
        for line in clif.lines().map(str::trim) {
            if let Some(label) = line.strip_prefix("block") {
                let id = label
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u32>()
                    .unwrap();
                blocks.insert(id, (line.contains(" cold:"), Vec::new()));
                current = Some(id);
            } else if let Some(id) = current {
                blocks.get_mut(&id).unwrap().1.push(line);
            }
        }
        let successors = |id: u32| {
            blocks[&id]
                .1
                .iter()
                .filter(|line| line.starts_with("brif ") || line.starts_with("jump "))
                .flat_map(|line| {
                    line.split("block").skip(1).map(|label| {
                        label
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>()
                            .parse::<u32>()
                            .unwrap()
                    })
                })
                .collect::<Vec<_>>()
        };
        let loop_blocks = blocks
            .keys()
            .copied()
            .filter(|&start| {
                let mut pending = successors(start);
                let mut seen = std::collections::BTreeSet::new();
                while let Some(id) = pending.pop() {
                    if blocks[&id].0 || !seen.insert(id) {
                        continue;
                    }
                    if id == start {
                        return true;
                    }
                    pending.extend(successors(id));
                }
                false
            })
            .collect::<std::collections::BTreeSet<_>>();
        // The rooted function's bytecode identity is the pointer loaded from
        // JSObject::u.func.function_bytecode (+48). Poll-budget and other
        // frame loads are legitimate inside the loop.
        let is_heap_identity_load = |line: &str| {
            line.split_once(" = ").is_some_and(|(_, instruction)| {
                instruction.starts_with("load.i64 ") && instruction.ends_with("+48")
            })
        };
        assert!(
            clif.lines().map(str::trim).any(is_heap_identity_load),
            "missing callee heap identity guard: {clif}"
        );
        let mut pending = vec![0];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let (cold, lines) = &blocks[&id];
            if *cold {
                continue;
            }
            for line in lines {
                if loop_blocks.contains(&id) {
                    assert!(
                        !is_heap_identity_load(line),
                        "invariant call target is reloaded inside the hot scalar loop: {clif}"
                    );
                    assert!(
                        !line.starts_with("store "),
                        "hot scalar loop writes interpreter frame: {clif}"
                    );
                    assert!(
                        !line.contains("fcvt_from_sint") && !line.contains("fadd"),
                        "integer induction uses floating arithmetic: {clif}"
                    );
                }
                assert!(
                    !line.contains("call_indirect"),
                    "hot inline path calls runtime: {clif}"
                );
                if line.starts_with("brif ") || line.starts_with("jump ") {
                    for label in line.split("block").skip(1) {
                        pending.push(
                            label
                                .chars()
                                .take_while(char::is_ascii_digit)
                                .collect::<String>()
                                .parse::<u32>()
                                .unwrap(),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn heap_operations_and_unknown_calls_keep_original_poll_frequency() {
    use rquickjs_jit::ir::ScalarNumericMode;
    for source in [
        "(function(a){return a.length})",
        "(function(a){return a[0]})",
        "(function(a){a.x=1;return 1})",
        "(function(a){a[0]=1;return 1})",
        "(function(f){return f()})",
    ] {
        let fixture = SnapshotFixture::compile(source);
        let body = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&body, 303).unwrap();
        let graph = ir.scalar_graph();
        // Even perfect numeric proofs cannot erase heap/call effects.
        assert!(
            !graph.permits_amortized_poll(
                ir.nodes(),
                &vec![Some(ScalarNumericMode::Int32); graph.values().len()],
            ),
            "{source}"
        );
    }
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn mixed_scalar_loops_amortize_polls_and_remain_interruptible() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    unsafe extern "C" {
        fn JS_JitGetHelperCount(
            rt: *mut rquickjs_core::qjs::JSRuntime,
            helper: u32,
            count: *mut u64,
        ) -> i32;
    }
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
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
        "function scalarPoll(n,a,enabled){for(let i=0;i<n;i++){a=enabled?a+1:a-1;}return a}"
    )
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("scalarPoll(4,1,true)"))
                .unwrap(),
            5
        );
        jit.poll();
    }
    assert!(jit.metrics().tier2_entries >= 10);
    let count = || {
        context.with(|ctx| {
            let mut count = 0;
            assert_eq!(
                unsafe {
                    JS_JitGetHelperCount(
                        rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()),
                        rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_POLL,
                        &mut count,
                    )
                },
                0
            );
            count
        })
    };
    let before = count();
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("scalarPoll(4096,1,true)"))
            .unwrap(),
        4097
    );
    let polls = count() - before;
    assert!(
        (64..=80).contains(&polls),
        "expected bounded poll batches, got {polls}"
    );
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<i32, _>("scalarPoll(128,0,false)"))
            .unwrap(),
        -128
    );
    let interrupts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&interrupts);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        seen.fetch_add(1, Ordering::SeqCst) >= 2
    })));
    let before = jit.metrics();
    assert!(context
        .with(|ctx| ctx.eval::<i32, _>("scalarPoll(1000000000,1,true)"))
        .is_err());
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries);
    assert!(interrupts.load(Ordering::SeqCst) >= 3);
    assert_eq!(after.native_entries, after.native_exits);
    runtime.set_interrupt_handler(None);
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn deferred_scalar_locals_restore_dirty_values_after_poll_and_overflow() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
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
    context.with(|ctx| ctx.eval::<(), _>(
        "function dirtyLocals(n,a,enabled){let value=a,last=0;for(let i=0;i<n;i++){last=value;value=enabled?value+1:value-1;}return value+last}"
    )).unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("dirtyLocals(2,3,true)"))
                .unwrap(),
            9
        );
        jit.poll();
    }
    let before = jit.metrics();
    assert!(before.tier2_entries >= 10);
    // The overflow occurs after a poll boundary; the last/value/i locals
    // have changed again since that publication and must be reconstructed.
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("dirtyLocals(128,2147483547,true)"))
            .unwrap(),
        4294967349.0
    );
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries);
    assert!(after.deopts > before.deopts);
    assert_eq!(after.native_entries, after.native_exits);
}

#[cfg(all(target_os = "macos", target_endian = "little"))]
#[test]
fn mixed_integer_update_overflow_recovers_without_wrapping() {
    use rquickjs::{Context, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(
        "function updateOverflow(n,enabled){let x=2147483646;for(let i=0;i<n;i++){x++}return enabled?x:0}"
    )).unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    while jit.metrics().tier2_entries < 10 && std::time::Instant::now() < deadline {
        assert_eq!(
            context
                .with(|ctx| ctx.eval::<i32, _>("updateOverflow(1,true)"))
                .unwrap(),
            2147483647
        );
        jit.poll();
    }
    let before = jit.metrics();
    assert!(before.tier2_entries >= 10);
    assert_eq!(
        context
            .with(|ctx| ctx.eval::<f64, _>("updateOverflow(3,true)"))
            .unwrap(),
        2147483649.0
    );
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries);
    assert!(after.deopts > before.deopts);
    assert_eq!(after.native_entries, after.native_exits);
}
