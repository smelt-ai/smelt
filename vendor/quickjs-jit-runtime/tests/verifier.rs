use rquickjs::{Context, Function, Runtime};
use rquickjs_jit::bytecode::{
    decode_raw, linked_opcode_table, opcode, CompileSnapshot, DecodeError, DeoptPoint,
    FallbackReason, OsrPoint, Resource, SlotKind, VerifierMetadata, VerifyErrorKind, VerifyLimits,
};

fn snapshot_from_parts(
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

fn verifier_metadata(osr: Vec<OsrPoint>, deopt: Vec<DeoptPoint>) -> VerifierMetadata {
    VerifierMetadata::new(osr, deopt)
}

fn i32_operand(value: i32) -> [u8; 4] {
    value.to_le_bytes()
}

#[test]
fn representative_typed_array_assignment_is_tier1_eligible() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../benchmarks/scripts/arrays-typed.js"
        ));
        ctx.eval::<(), _>(source).unwrap();
        let function: Function = ctx.globals().get("workload").unwrap();
        let snapshot = unsafe {
            CompileSnapshot::capture_raw(ctx.as_raw().as_ptr(), function.as_value().as_raw())
        }
        .unwrap();
        let verified = snapshot
            .verify(VerifyLimits::default())
            .expect("representative typed-array bytecode is structurally valid");
        verified
            .tier1_eligibility()
            .expect("representative typed-array bytecode is Tier 1 eligible");
    });
}

#[test]
fn decoder_rejects_unknown_opcodes_and_never_panics_on_arbitrary_bytes() {
    assert_eq!(
        decode_raw(&[u8::MAX]).unwrap_err(),
        DecodeError::UnknownOpcode {
            pc: 0,
            opcode: u8::MAX
        }
    );

    let mut state = 0x9e37_79b9_u32;
    for len in 0..=256 {
        let mut bytes = vec![0; len];
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        let result = std::panic::catch_unwind(|| decode_raw(&bytes));
        assert!(result.is_ok(), "decoder panicked for {bytes:02x?}");
        if let Ok(instructions) = result.unwrap() {
            let consumed: usize = instructions
                .iter()
                .map(|instruction| instruction.size())
                .sum();
            assert_eq!(consumed, bytes.len());
        }
    }
}

#[test]
fn verifier_never_panics_on_arbitrary_untrusted_snapshots() {
    for mut state in [0x243f_6a88_u32, 0xa341_316c, 0x9e37_79b9, 0xdead_beef] {
        for len in 0..=512 {
            let mut bytes = vec![0; len];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = state as u8;
            }
            let snapshot = snapshot_from_parts(
                bytes.clone(),
                state as u16 & 7,
                (state >> 3) as u16 & 7,
                (state >> 6) as u16 & 7,
                (state >> 9) & 7,
            );
            let result = std::panic::catch_unwind(|| snapshot.verify(VerifyLimits::default()));
            assert!(result.is_ok(), "verifier panicked for {bytes:02x?}");
        }
    }

    for opcode in linked_opcode_table() {
        let mut complete = vec![0; opcode.size()];
        complete[0] = opcode.id();
        complete.push(opcode::RETURN_UNDEF);
        let result = std::panic::catch_unwind(|| {
            snapshot_from_parts(complete, 4, 4, 4, 4).verify(VerifyLimits::default())
        });
        assert!(result.is_ok(), "verifier panicked for {}", opcode.name());
    }
}

#[test]
fn verifier_rejects_a_branch_into_an_operand() {
    let mut bytes = vec![opcode::PUSH_I32];
    bytes.extend(i32_operand(7));
    bytes.push(opcode::GOTO8);
    bytes.push((-5_i8) as u8); // operand PC 6 - 5 = byte 1, inside PUSH_I32
    let error = snapshot_from_parts(bytes, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(error.pc(), 5);
    assert_eq!(
        error.kind(),
        &VerifyErrorKind::BranchTargetInsideInstruction { target: 1 }
    );
}

#[test]
fn verifier_rejects_stack_underflow_and_inconsistent_merge_height() {
    let underflow = snapshot_from_parts(vec![opcode::ADD, opcode::RETURN], 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(underflow.pc(), 0);
    assert_eq!(
        underflow.kind(),
        &VerifyErrorKind::StackUnderflow {
            needed: 2,
            available: 0
        }
    );

    let bytes = vec![
        opcode::PUSH_TRUE,
        opcode::IF_FALSE8,
        3, // operand PC 2 + 3 = RETURN_UNDEF at 5
        opcode::PUSH_TRUE,
        opcode::NOP,
        opcode::RETURN_UNDEF,
    ];
    let merge = snapshot_from_parts(bytes, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(merge.pc(), 5);
    assert_eq!(
        merge.kind(),
        &VerifyErrorKind::InconsistentMergeHeight {
            expected: 0,
            actual: 1
        }
    );
}

#[test]
fn verifier_rejects_incompatible_merge_kinds_without_a_boxing_join() {
    let bytes = vec![
        opcode::PUSH_TRUE,
        opcode::IF_FALSE8,
        5, // operand PC 2 + 5 = PUSH_TRUE at 7
        opcode::PUSH_I8,
        1,
        opcode::GOTO8,
        3, // operand PC 6 + 3 = RETURN at 9
        opcode::PUSH_TRUE,
        opcode::NOP,
        opcode::RETURN,
    ];
    let error = snapshot_from_parts(bytes, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(error.pc(), 9);
    assert!(matches!(
        error.kind(),
        VerifyErrorKind::IncompatibleMergeKind { slot: 0, .. }
    ));
}

#[test]
fn explicit_boxing_on_both_paths_allows_a_tagged_join() {
    let bytes = vec![
        opcode::PUSH_TRUE,
        opcode::IF_FALSE8,
        6, // operand PC 2 + 6 = PUSH_TRUE at 8
        opcode::PUSH_I8,
        1,
        opcode::PLUS,
        opcode::GOTO8,
        3, // operand PC 7 + 3 = RETURN at 10
        opcode::PUSH_TRUE,
        opcode::PLUS,
        opcode::RETURN,
    ];
    assert!(snapshot_from_parts(bytes, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .is_ok());
}

#[test]
fn to_propkey_preserves_numeric_stack_proof_across_null_base_bypass() {
    let opcode_named = |name: &str| {
        linked_opcode_table()
            .find(|opcode| opcode.name() == name)
            .unwrap_or_else(|| panic!("linked opcode {name}"))
            .id()
    };
    let bytes = vec![
        opcode::PUSH_I8,
        1,
        opcode::PUSH_TRUE,
        opcode::DUP,
        opcode_named("is_undefined_or_null"),
        opcode_named("if_true8"),
        4, // operand PC 6 + 4 = common SWAP at PC 10
        opcode_named("swap"),
        opcode_named("to_propkey"),
        opcode_named("swap"),
        opcode_named("swap"),
        opcode::DROP,
        opcode::RETURN,
    ];
    assert!(snapshot_from_parts(bytes, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .is_ok());
}

#[test]
fn stack_copy_operations_do_not_implicitly_box_specialized_slots() {
    let bytes = vec![
        opcode::PUSH_TRUE,
        opcode::IF_FALSE8,
        7, // operand PC 2 + 7 = PUSH_TRUE at 9
        opcode::PUSH_I8,
        1,
        opcode::DUP,
        opcode::DROP,
        opcode::GOTO8,
        2, // operand PC 8 + 2 = RETURN at 10
        opcode::PUSH_TRUE,
        opcode::RETURN,
    ];
    let error = snapshot_from_parts(bytes, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(error.pc(), 10);
    assert!(matches!(
        error.kind(),
        VerifyErrorKind::IncompatibleMergeKind { slot: 0, .. }
    ));
}

#[test]
fn local_kind_join_forgets_numeric_proof_at_the_tagged_frame_boundary() {
    let bytes = vec![
        opcode::PUSH_TRUE,
        opcode::IF_FALSE8,
        8, // operand PC 2 + 8 = NOP at 10
        opcode::PUSH_I8,
        1,
        opcode::PUT_LOC,
        0,
        0,
        opcode::GOTO8,
        2, // operand PC 9 + 2 = RETURN_UNDEF at 11
        opcode::NOP,
        opcode::RETURN_UNDEF,
    ];
    let snapshot = snapshot_from_parts(bytes, 0, 1, 0, 0).with_metadata(verifier_metadata(
        vec![],
        vec![DeoptPoint::new(
            11,
            vec![SlotKind::Tagged],
            vec![SlotKind::Tagged],
        )],
    ));

    // The local is a tagged interpreter-frame cell on both paths. The Int32
    // kind is only a path-local proof, so the merge must forget that proof and
    // expose Tagged in the deopt map rather than inventing a stack conversion.
    assert!(snapshot.verify(VerifyLimits::default()).is_ok());
}

#[test]
fn verifier_checks_local_arg_closure_and_constant_indices() {
    let fixtures = [
        (
            vec![opcode::GET_LOC, 1, 0, opcode::RETURN],
            VerifyErrorKind::LocalIndexOutOfRange { index: 1, count: 1 },
        ),
        (
            vec![opcode::GET_ARG, 1, 0, opcode::RETURN],
            VerifyErrorKind::ArgumentIndexOutOfRange { index: 1, count: 1 },
        ),
        (
            vec![opcode::GET_VAR_REF, 1, 0, opcode::RETURN],
            VerifyErrorKind::ClosureIndexOutOfRange { index: 1, count: 1 },
        ),
        (
            vec![opcode::PUSH_CONST8, 1, opcode::RETURN],
            VerifyErrorKind::ConstantIndexOutOfRange { index: 1, count: 1 },
        ),
        (
            vec![opcode::GET_LOC0_LOC1, opcode::RETURN],
            VerifyErrorKind::LocalIndexOutOfRange { index: 1, count: 1 },
        ),
    ];

    for (bytes, expected) in fixtures {
        let error = snapshot_from_parts(bytes, 1, 1, 1, 1)
            .verify(VerifyLimits::default())
            .unwrap_err();
        assert_eq!(error.kind(), &expected);
    }
}

#[test]
fn compact_double_local_preserves_its_authoritative_stack_effect() {
    let snapshot = snapshot_from_parts(
        vec![opcode::GET_LOC0_LOC1, opcode::ADD, opcode::RETURN],
        0,
        2,
        0,
        0,
    );
    assert!(snapshot.verify(VerifyLimits::default()).is_ok());
}

#[test]
fn set_local_uninitialized_records_the_conservative_slot_kind() {
    let metadata = verifier_metadata(
        vec![],
        vec![DeoptPoint::new(
            0,
            vec![SlotKind::Tagged],
            vec![SlotKind::Uninitialized],
        )],
    );
    let snapshot = snapshot_from_parts(
        vec![opcode::SET_LOC_UNINITIALIZED, 0, 0, opcode::RETURN_UNDEF],
        0,
        1,
        0,
        0,
    )
    .with_metadata(metadata);
    assert!(snapshot.verify(VerifyLimits::default()).is_ok());
}

#[test]
fn entry_locals_start_as_tagged_undefined_values() {
    let metadata = verifier_metadata(
        vec![],
        vec![DeoptPoint::new(
            0,
            vec![SlotKind::Tagged],
            vec![SlotKind::Tagged],
        )],
    );
    let snapshot = snapshot_from_parts(vec![opcode::NOP, opcode::RETURN_UNDEF], 0, 1, 0, 0)
        .with_metadata(metadata);
    assert!(snapshot.verify(VerifyLimits::default()).is_ok());
}

#[test]
fn mixed_output_opcodes_record_catch_and_uninitialized_slots() {
    let for_of = snapshot_from_parts(
        vec![opcode::PUSH_TRUE, opcode::FOR_OF_START, opcode::RETURN],
        0,
        0,
        0,
        0,
    )
    .with_metadata(verifier_metadata(
        vec![],
        vec![DeoptPoint::new(
            1,
            vec![SlotKind::Tagged],
            vec![SlotKind::Tagged, SlotKind::Tagged, SlotKind::CatchOffset],
        )],
    ));
    assert!(for_of.verify(VerifyLimits::default()).is_ok());

    let using_init =
        snapshot_from_parts(vec![opcode::USING_DISPOSE_INIT, opcode::RETURN], 0, 0, 0, 0)
            .with_metadata(verifier_metadata(
                vec![],
                vec![DeoptPoint::new(0, vec![], vec![SlotKind::Uninitialized])],
            ));
    assert!(using_init.verify(VerifyLimits::default()).is_ok());
}

#[test]
fn local_arithmetic_updates_the_local_slot_kind() {
    let cases = [
        vec![
            opcode::PUSH_I8,
            1,
            opcode::PUT_LOC,
            0,
            0,
            opcode::INC_LOC,
            0,
            opcode::NOP,
            opcode::RETURN_UNDEF,
        ],
        vec![
            opcode::PUSH_I8,
            1,
            opcode::PUT_LOC,
            0,
            0,
            opcode::DEC_LOC,
            0,
            opcode::NOP,
            opcode::RETURN_UNDEF,
        ],
        vec![
            opcode::PUSH_I8,
            1,
            opcode::PUT_LOC,
            0,
            0,
            opcode::PUSH_I8,
            2,
            opcode::ADD_LOC,
            0,
            opcode::NOP,
            opcode::RETURN_UNDEF,
        ],
    ];

    for bytes in cases {
        let pc = bytes.iter().rposition(|byte| *byte == opcode::NOP).unwrap() as u32;
        let snapshot = snapshot_from_parts(bytes, 0, 1, 0, 0).with_metadata(verifier_metadata(
            vec![],
            vec![DeoptPoint::new(
                pc,
                vec![SlotKind::Tagged],
                vec![SlotKind::Tagged],
            )],
        ));
        assert!(snapshot.verify(VerifyLimits::default()).is_ok());
    }
}

#[test]
fn verifier_checks_indices_embedded_after_atom_operands() {
    let fixtures = [
        (
            opcode::MAKE_LOC_REF,
            VerifyErrorKind::LocalIndexOutOfRange { index: 1, count: 1 },
        ),
        (
            opcode::MAKE_ARG_REF,
            VerifyErrorKind::ArgumentIndexOutOfRange { index: 1, count: 1 },
        ),
        (
            opcode::MAKE_VAR_REF_REF,
            VerifyErrorKind::ClosureIndexOutOfRange { index: 1, count: 1 },
        ),
    ];

    for (opcode, expected) in fixtures {
        let bytes = vec![opcode, 0, 0, 0, 0, 1, 0, opcode::RETURN];
        let error = snapshot_from_parts(bytes, 1, 1, 1, 0)
            .verify(VerifyLimits::default())
            .unwrap_err();
        assert_eq!(error.kind(), &expected);
    }
}

#[test]
fn using_dispose_validates_its_adjacent_local_pair() {
    for dispose in [opcode::USING_DISPOSE, opcode::USING_DISPOSE_ASYNC] {
        let mut bytes = vec![opcode::USING_DISPOSE_INIT, dispose, 0, 0];
        bytes.push(opcode::RETURN);
        let error = snapshot_from_parts(bytes, 0, 1, 0, 0)
            .verify(VerifyLimits::default())
            .unwrap_err();
        assert_eq!(
            error.kind(),
            &VerifyErrorKind::LocalIndexOutOfRange { index: 1, count: 1 }
        );
    }
}

#[test]
fn well_formedness_and_tier1_eligibility_are_separate() {
    let fixtures = [
        (
            vec![
                opcode::PUSH_UNDEFINED,
                opcode::EVAL,
                0,
                0,
                0,
                0,
                opcode::RETURN,
            ],
            FallbackReason::DirectEval,
        ),
        (
            vec![
                opcode::PUSH_UNDEFINED,
                opcode::WITH_GET_VAR,
                0,
                0,
                0,
                0,
                5,
                0,
                0,
                0,
                0,
                opcode::RETURN_UNDEF,
            ],
            FallbackReason::WithScope,
        ),
        (
            vec![opcode::INITIAL_YIELD, opcode::RETURN_UNDEF],
            FallbackReason::Generator,
        ),
        (
            vec![opcode::PUSH_UNDEFINED, opcode::AWAIT, opcode::RETURN],
            FallbackReason::Async,
        ),
    ];

    for (bytes, reason) in fixtures {
        let verified = snapshot_from_parts(bytes, 0, 0, 0, 0)
            .verify(VerifyLimits::default())
            .expect("feature policy must not be a bytecode well-formedness error");
        let rejection = verified.tier1_eligibility().unwrap_err();
        assert_eq!(
            verified.instructions()[rejection.pc() as usize]
                .opcode()
                .name(),
            match reason {
                FallbackReason::DirectEval => "eval",
                FallbackReason::WithScope => "with_get_var",
                FallbackReason::Generator => "initial_yield",
                FallbackReason::Async => "await",
                _ => unreachable!(),
            }
        );
        assert_eq!(rejection.reason(), reason);
    }
}

#[test]
fn exception_regions_are_tier1_eligible_with_exact_handler_states() {
    // catch L7; drop; return_undef; L7: drop; return_undef
    let verified = snapshot_from_parts(
        vec![
            opcode::CATCH,
            6,
            0,
            0,
            0,
            opcode::DROP,
            opcode::RETURN_UNDEF,
            opcode::DROP,
            opcode::RETURN_UNDEF,
        ],
        0,
        0,
        0,
        0,
    )
    .verify(VerifyLimits::default())
    .unwrap();
    assert!(verified.tier1_eligibility().is_ok());
    let handler = verified
        .exception_handler(5)
        .expect("drop is inside the region");
    assert_eq!(handler.catch_index(), 0);
    assert_eq!(handler.handler_pc(), Some(7));
    assert!(verified.exception_handler(7).is_none());
    assert!(verified.exception_handler(0).is_none());
}

#[test]
fn a_catch_whose_handler_is_its_fallthrough_is_rejected() {
    let error = snapshot_from_parts(vec![opcode::CATCH, 4, 0, 0, 0, opcode::RETURN], 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(error.kind(), &VerifyErrorKind::UnsupportedExceptionRegion);
}

#[test]
fn structurally_valid_captured_exception_map_is_eligibility_not_verification() {
    let snapshot = snapshot_from_parts(vec![opcode::RETURN_UNDEF], 0, 0, 0, 0)
        .with_exception_map(vec![0, 0, 0, 0]);
    let verified = snapshot
        .verify(VerifyLimits::default())
        .expect("exception metadata is well-formed bytecode metadata");
    let rejection = verified.tier1_eligibility().unwrap_err();
    assert_eq!(rejection.reason(), FallbackReason::ExceptionRegion);
}

#[test]
fn verifier_rejects_invalid_osr_and_deopt_metadata_before_ir() {
    let osr = verifier_metadata(vec![OsrPoint::new(1, vec![SlotKind::Tagged])], vec![]);
    let error = snapshot_from_parts(vec![opcode::NOP, opcode::RETURN_UNDEF], 0, 0, 0, 0)
        .with_metadata(osr)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(error.kind(), &VerifyErrorKind::OsrPointNotLoopHeader);

    let deopt = verifier_metadata(vec![], vec![DeoptPoint::new(0, vec![], vec![])]);
    let error = snapshot_from_parts(vec![opcode::PUSH_TRUE, opcode::RETURN], 0, 0, 0, 0)
        .with_metadata(deopt)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(error.kind(), &VerifyErrorKind::IncompleteDeoptState);
}

#[test]
fn every_verifier_resource_has_a_distinct_limit_error() {
    let base = snapshot_from_parts(vec![opcode::NOP, opcode::RETURN_UNDEF], 0, 0, 0, 0);
    let cases = [
        (
            VerifyLimits {
                max_snapshot_bytes: 1,
                ..VerifyLimits::default()
            },
            Resource::SnapshotBytes,
        ),
        (
            VerifyLimits {
                max_instructions: 1,
                ..VerifyLimits::default()
            },
            Resource::DecodedInstructions,
        ),
        (
            VerifyLimits {
                max_basic_blocks: 0,
                ..VerifyLimits::default()
            },
            Resource::BasicBlocks,
        ),
        (
            VerifyLimits {
                max_metadata_bytes: 0,
                ..VerifyLimits::default()
            },
            Resource::MetadataBytes,
        ),
        (
            VerifyLimits {
                max_work_units: 1,
                ..VerifyLimits::default()
            },
            Resource::WorkUnits,
        ),
    ];

    for (limits, resource) in cases {
        let error = base.verify(limits).unwrap_err();
        assert_eq!(error.kind(), &VerifyErrorKind::ResourceLimit { resource });
    }
}

#[test]
fn instruction_limit_stops_decoding_before_later_malformed_bytes() {
    let snapshot = snapshot_from_parts(vec![opcode::NOP, u8::MAX], 0, 0, 0, 0);
    let error = snapshot
        .verify(VerifyLimits {
            max_instructions: 0,
            ..VerifyLimits::default()
        })
        .unwrap_err();
    assert_eq!(
        error.kind(),
        &VerifyErrorKind::ResourceLimit {
            resource: Resource::DecodedInstructions,
        }
    );
}

#[test]
fn large_frame_proof_is_charged_by_state_cells() {
    let mut bytecode = vec![opcode::NOP; 20_000];
    bytecode.push(opcode::RETURN_UNDEF);
    let snapshot = snapshot_from_parts(bytecode, 0, u16::MAX, 0, 0);
    let error = snapshot.verify(VerifyLimits::default()).unwrap_err();
    assert_eq!(
        error.kind(),
        &VerifyErrorKind::ResourceLimit {
            resource: Resource::WorkUnits,
        }
    );
}

#[test]
fn verifier_rejects_empty_and_unterminated_bytecode() {
    let empty = snapshot_from_parts(vec![], 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(empty.pc(), 0);
    assert_eq!(empty.kind(), &VerifyErrorKind::EmptyBytecode);

    let unterminated = snapshot_from_parts(vec![opcode::NOP], 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(unterminated.pc(), 0);
    assert_eq!(unterminated.kind(), &VerifyErrorKind::MissingTerminator);
}

#[test]
fn verifier_rejects_unreachable_orphan_instructions() {
    let error = snapshot_from_parts(vec![opcode::RETURN_UNDEF, opcode::ADD], 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .unwrap_err();
    assert_eq!(error.pc(), 1);
    assert_eq!(error.kind(), &VerifyErrorKind::UnreachableInstruction);
}

#[test]
fn throw_error_is_a_terminal_instruction() {
    let bytecode = vec![opcode::THROW_ERROR, 0, 0, 0, 0, 0];
    assert!(snapshot_from_parts(bytecode, 0, 0, 0, 0)
        .verify(VerifyLimits::default())
        .is_ok());
}
