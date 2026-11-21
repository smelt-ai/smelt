use rquickjs_jit::ir::{
    DeoptMap, DeoptOwnership, DeoptPhase, Materialization, MaterializedValue, OptimizedFrameShape,
    TaggedValue,
};
use rquickjs_jit::{
    bytecode::{opcode, CompileSnapshot, VerifyLimits},
    ir::OptimizedIr,
};

#[test]
fn complete_deopt_map_materializes_every_slot_once() {
    let shape = OptimizedFrameShape::new(2, 3, 2);
    let map = DeoptMap::new(
        5,
        42,
        DeoptPhase::AfterEffect(9),
        vec![
            Materialization::argument(0, MaterializedValue::TaggedSlot(0)),
            Materialization::argument(1, MaterializedValue::Int32(7)),
            Materialization::local(0, MaterializedValue::Float64(-0.0)),
            Materialization::local(1, MaterializedValue::Undefined),
            Materialization::local(2, MaterializedValue::Null),
            Materialization::stack(0, MaterializedValue::Bool(true)),
            Materialization::stack(1, MaterializedValue::TaggedSlot(6)),
        ],
    );

    assert!(map.validate(shape).is_ok());
    let frame = map.materialize(shape).expect("complete map materializes");
    assert_eq!(frame.resume_pc(), 42);
    assert_eq!(frame.side_effect_epoch(), 9);
    assert_eq!(frame.slots().len(), 7);
    assert!(frame.slots()[2].is_negative_zero());
}

#[test]
fn malformed_deopt_maps_fail_closed_before_writing_any_slot() {
    let shape = OptimizedFrameShape::new(1, 1, 0);
    let duplicate = DeoptMap::new(
        1,
        8,
        DeoptPhase::BeforeEffect(3),
        vec![
            Materialization::argument(0, MaterializedValue::Int32(1)),
            Materialization::argument(0, MaterializedValue::Int32(2)),
        ],
    );
    let mut destination = vec![MaterializedValue::Poison; shape.slot_count()];
    assert!(duplicate.materialize_into(shape, &mut destination).is_err());
    assert_eq!(destination, vec![MaterializedValue::Poison; 2]);
}

#[test]
fn effect_phase_selects_exact_resume_boundary() {
    assert_eq!(DeoptPhase::BeforeEffect(4).side_effect_epoch(), 3);
    assert_eq!(DeoptPhase::AfterEffect(4).side_effect_epoch(), 4);
}

#[derive(Default)]
struct Refcounts {
    duplicated: Vec<u16>,
    released: Vec<u64>,
    fail_on: Option<u16>,
}

impl DeoptOwnership for Refcounts {
    type Error = ();
    fn duplicate(&mut self, source_slot: u16) -> Result<TaggedValue, Self::Error> {
        if self.fail_on == Some(source_slot) {
            return Err(());
        }
        self.duplicated.push(source_slot);
        Ok(TaggedValue::new(u64::from(source_slot), 7))
    }
    fn release(&mut self, value: TaggedValue) {
        self.released.push(value.payload);
    }
}

#[test]
fn owning_values_are_duplicated_before_publish_and_rolled_back_on_failure() {
    let shape = OptimizedFrameShape::new(1, 1, 0);
    let map = DeoptMap::new(
        1,
        6,
        DeoptPhase::AfterEffect(1),
        vec![
            Materialization::argument(0, MaterializedValue::TaggedSlot(3)),
            Materialization::local(0, MaterializedValue::TaggedSlot(4)),
        ],
    );
    let mut refs = Refcounts {
        fail_on: Some(4),
        ..Default::default()
    };
    assert!(map.materialize_owned(shape, &mut refs).is_err());
    assert_eq!(refs.duplicated, vec![3]);
    assert_eq!(refs.released, vec![3]);

    refs.fail_on = None;
    let frame = map.materialize_owned(shape, &mut refs).unwrap();
    assert_eq!(frame.owned_count(), 2);
}

#[test]
fn production_identity_recipes_execute_arguments_and_locals_in_place() {
    let shape = OptimizedFrameShape::new(1, 1, 0);
    let map = DeoptMap::new(
        3,
        9,
        DeoptPhase::BeforeEffect(0),
        vec![
            Materialization::argument(0, MaterializedValue::TaggedSlot(0)),
            Materialization::local(0, MaterializedValue::TaggedSlot(1)),
        ],
    );
    assert!(map.validate_identity_materialization(shape).is_ok());

    let non_identity = DeoptMap::new(
        3,
        9,
        DeoptPhase::BeforeEffect(0),
        vec![
            Materialization::argument(0, MaterializedValue::TaggedSlot(1)),
            Materialization::local(0, MaterializedValue::TaggedSlot(0)),
        ],
    );
    assert!(non_identity
        .validate_identity_materialization(shape)
        .is_err());
}

#[test]
fn arithmetic_guard_resumes_at_operation_with_both_operands_materialized() {
    let verified = CompileSnapshot::from_untrusted_bytecode(
        vec![
            opcode::GET_ARG,
            0,
            0,
            opcode::GET_ARG,
            1,
            0,
            opcode::ADD,
            opcode::RETURN,
        ],
        2,
        0,
        0,
        0,
    )
    .verify(VerifyLimits::default())
    .unwrap();
    let ir = OptimizedIr::translate(&verified, 1).unwrap();
    let site = ir
        .guard_maps()
        .iter()
        .find(|site| site.map().resume_pc() == 6)
        .expect("add overflow has an instruction-local deopt site");
    let add = ir.nodes().iter().find(|node| node.pc() == 6).unwrap();

    assert_eq!(add.deopt_guard(), Some(site.guard()));
    assert_ne!(site.guard(), ir.guard_maps()[0].guard());
    assert_eq!(site.map().phase(), DeoptPhase::BeforeEffect(1));
    assert_eq!(site.shape(), OptimizedFrameShape::new(2, 0, 2));
    assert!(site
        .map()
        .validate_identity_materialization(site.shape())
        .is_ok());
    let frame = site.map().materialize(site.shape()).unwrap();
    assert_eq!(frame.resume_pc(), 6);
    assert_eq!(
        frame.slots(),
        &[
            MaterializedValue::TaggedSlot(0),
            MaterializedValue::TaggedSlot(1),
            MaterializedValue::TaggedSlot(2),
            MaterializedValue::TaggedSlot(3),
        ]
    );
}

#[test]
fn checked_integer_arithmetic_sites_have_exact_pre_effect_deopt_maps() {
    for arithmetic_opcode in [
        rquickjs_core::qjs::QJS_JIT_OP_SUB,
        rquickjs_core::qjs::QJS_JIT_OP_MUL,
        rquickjs_core::qjs::QJS_JIT_OP_DIV,
    ] {
        let verified = CompileSnapshot::from_untrusted_bytecode(
            vec![
                opcode::GET_ARG,
                0,
                0,
                opcode::GET_ARG,
                1,
                0,
                arithmetic_opcode,
                opcode::RETURN,
            ],
            2,
            0,
            0,
            0,
        )
        .verify(VerifyLimits::default())
        .unwrap();
        let ir = OptimizedIr::translate(&verified, 2).unwrap();
        let node = ir.nodes().iter().find(|node| node.pc() == 6).unwrap();
        let site = ir
            .guard_maps()
            .iter()
            .find(|site| Some(site.guard()) == node.deopt_guard())
            .unwrap();

        assert_eq!(site.map().resume_pc(), 6);
        assert_eq!(site.map().phase(), DeoptPhase::BeforeEffect(1));
        assert_eq!(site.shape(), OptimizedFrameShape::new(2, 0, 2));
        assert!(site
            .map()
            .validate_identity_materialization(site.shape())
            .is_ok());
    }
}

// ---------------------------------------------------------------------------
// M2 core opcodes: exact guard sites in the optimized IR.
// ---------------------------------------------------------------------------

fn translate(bytecode: Vec<u8>, arg_count: u16, local_count: u16) -> OptimizedIr {
    let verified = CompileSnapshot::from_untrusted_bytecode(bytecode, arg_count, local_count, 0, 0)
        .verify(VerifyLimits::default())
        .expect("synthetic bytecode verifies");
    OptimizedIr::translate(&verified, 3).expect("M2 opcodes translate")
}

fn bytecode_name(node: &rquickjs_jit::ir::OptimizedNode) -> &str {
    match node.kind() {
        rquickjs_jit::ir::OptimizedNodeKind::Bytecode { opcode } => opcode,
        other => panic!("expected a bytecode node, found {other:?}"),
    }
}

/// The node at `pc` owns an instruction-local, pre-effect identity guard
/// site whose frame shape is exactly the operand stack before the pops.
fn assert_exact_guard(ir: &OptimizedIr, pc: u32, shape: OptimizedFrameShape) {
    let node = ir
        .nodes()
        .iter()
        .find(|node| {
            node.pc() == pc
                && node.deopt_guard().is_some()
                && matches!(
                    node.kind(),
                    rquickjs_jit::ir::OptimizedNodeKind::Bytecode { .. }
                )
        })
        .unwrap_or_else(|| panic!("{} at pc {pc} carries no guard", ir.nodes().len()));
    let guard = node.deopt_guard().unwrap();
    assert_ne!(guard, ir.guard_maps()[0].guard(), "{}", bytecode_name(node));
    let site = ir
        .guard_maps()
        .iter()
        .find(|site| site.guard() == guard)
        .unwrap();
    assert_eq!(site.map().resume_pc(), pc, "{}", bytecode_name(node));
    assert_eq!(
        site.map().phase(),
        DeoptPhase::BeforeEffect(1),
        "{}",
        bytecode_name(node)
    );
    assert_eq!(site.shape(), shape, "{}", bytecode_name(node));
    assert!(
        site.map()
            .validate_identity_materialization(site.shape())
            .is_ok(),
        "{}",
        bytecode_name(node)
    );
}

#[test]
fn tail_call_expands_into_a_guarded_call_and_a_return_at_the_same_pc() {
    use rquickjs_core::qjs;
    let ir = translate(
        vec![
            opcode::GET_ARG,
            0,
            0,
            opcode::GET_ARG,
            1,
            0,
            qjs::QJS_JIT_OP_TAIL_CALL,
            1,
            0,
        ],
        2,
        0,
    );
    let expanded = ir
        .nodes()
        .iter()
        .filter(|node| node.pc() == 6)
        .collect::<Vec<_>>();
    assert_eq!(expanded.len(), 2);
    assert_eq!(bytecode_name(expanded[0]), "call");
    assert_eq!((expanded[0].pops(), expanded[0].pushes()), (2, 1));
    assert_eq!(bytecode_name(expanded[1]), "return");
    assert_eq!((expanded[1].pops(), expanded[1].pushes()), (1, 0));
    assert_eq!(expanded[1].deopt_guard(), None);
    assert_exact_guard(&ir, 6, OptimizedFrameShape::new(2, 0, 2));

    let ir = translate(
        vec![
            opcode::GET_ARG,
            0,
            0,
            opcode::GET_ARG,
            1,
            0,
            opcode::GET_ARG,
            2,
            0,
            qjs::QJS_JIT_OP_TAIL_CALL_METHOD,
            1,
            0,
        ],
        3,
        0,
    );
    let expanded = ir
        .nodes()
        .iter()
        .filter(|node| node.pc() == 9)
        .collect::<Vec<_>>();
    assert_eq!(bytecode_name(expanded[0]), "call_method");
    assert_eq!((expanded[0].pops(), expanded[0].pushes()), (3, 1));
    assert_eq!(bytecode_name(expanded[1]), "return");
    assert_exact_guard(&ir, 9, OptimizedFrameShape::new(3, 0, 3));
}

#[test]
fn m2_numeric_opcodes_own_instruction_local_deopt_sites() {
    use rquickjs_core::qjs;
    for unary in [
        qjs::QJS_JIT_OP_NEG,
        qjs::QJS_JIT_OP_PLUS,
        qjs::QJS_JIT_OP_NOT,
        qjs::QJS_JIT_OP_LNOT,
        qjs::QJS_JIT_OP_INC,
        qjs::QJS_JIT_OP_DEC,
        qjs::QJS_JIT_OP_POST_INC,
        qjs::QJS_JIT_OP_POST_DEC,
    ] {
        let ir = translate(vec![opcode::GET_ARG, 0, 0, unary, opcode::RETURN], 1, 0);
        assert_exact_guard(&ir, 3, OptimizedFrameShape::new(1, 0, 1));
    }
    for binary in [
        qjs::QJS_JIT_OP_MOD,
        qjs::QJS_JIT_OP_SHL,
        qjs::QJS_JIT_OP_SAR,
        qjs::QJS_JIT_OP_SHR,
        qjs::QJS_JIT_OP_EQ,
        qjs::QJS_JIT_OP_NEQ,
        qjs::QJS_JIT_OP_STRICT_EQ,
        qjs::QJS_JIT_OP_STRICT_NEQ,
    ] {
        let ir = translate(
            vec![
                opcode::GET_ARG,
                0,
                0,
                opcode::GET_ARG,
                1,
                0,
                binary,
                opcode::RETURN,
            ],
            2,
            0,
        );
        assert_exact_guard(&ir, 6, OptimizedFrameShape::new(2, 0, 2));
    }
    for local in [opcode::INC_LOC, opcode::DEC_LOC] {
        let ir = translate(vec![local, 0, opcode::GET_LOC, 0, 0, opcode::RETURN], 0, 1);
        assert_exact_guard(&ir, 0, OptimizedFrameShape::new(0, 1, 0));
    }
    let ir = translate(
        vec![
            opcode::GET_ARG,
            0,
            0,
            opcode::ADD_LOC,
            0,
            opcode::GET_LOC,
            0,
            0,
            opcode::RETURN,
        ],
        1,
        1,
    );
    assert_exact_guard(&ir, 3, OptimizedFrameShape::new(1, 1, 1));
}

#[test]
fn constants_and_stack_shuffles_carry_no_guard_but_frame_stores_do() {
    use rquickjs_core::qjs;
    let ir = translate(
        vec![
            qjs::QJS_JIT_OP_PUSH_MINUS1,
            qjs::QJS_JIT_OP_NULL,
            qjs::QJS_JIT_OP_PUSH_TRUE,
            qjs::QJS_JIT_OP_ROT3L,
            qjs::QJS_JIT_OP_NIP1,
            qjs::QJS_JIT_OP_PUT_ARG0,
            qjs::QJS_JIT_OP_SET_LOC,
            0,
            0,
            qjs::QJS_JIT_OP_IS_NULL,
            opcode::RETURN,
        ],
        1,
        1,
    );
    // Stores into interpreter-owned argument/local buffers keep an exact
    // deoptimization site: a value of unproven representation is checked for
    // a heap reference before its borrowed alias is spilled into the slot.
    for node in ir.nodes().iter().filter(|node| {
        matches!(
            node.kind(),
            rquickjs_jit::ir::OptimizedNodeKind::Bytecode { .. }
        )
    }) {
        let rquickjs_jit::ir::OptimizedNodeKind::Bytecode { opcode } = node.kind() else {
            unreachable!()
        };
        let is_store = opcode.starts_with("put_arg") || opcode.starts_with("set_loc");
        assert_eq!(node.deopt_guard().is_some(), is_store, "{opcode}");
    }
    assert_eq!(
        ir.guard_maps().len(),
        3,
        "the entry guard plus one site per frame store"
    );
    assert_eq!(ir.max_stack(), 3);
}
