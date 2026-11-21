#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use cranelift_codegen::{isa, settings};
use rquickjs_core::qjs;
use rquickjs_jit::{
    bytecode::{linked_opcode_table, opcode, FallbackReason, VerifyLimits},
    code_cache::{FrameStateLocationKind, RelocationTarget},
    compiler::{baseline::BaselineCompiler, CompileFailure},
    ir::{BaselineIr, IrOp, PollKind},
    platform::CodeMemoryError,
    runtime::{FeedbackTable, FunctionKey, ObservedType},
    test_support::{
        compile_implemented_fixture, verified_bytecode, JSValueRepr, SnapshotFixture,
        SyntheticFrame,
    },
};
use std::collections::BTreeSet;

fn named_opcode(name: &str) -> u8 {
    linked_opcode_table()
        .find(|opcode| opcode.name() == name)
        .unwrap_or_else(|| panic!("linked QuickJS opcode {name}"))
        .id()
}

#[test]
fn baseline_monomorphic_call_has_guarded_unboxed_direct_edge_and_helper_miss() {
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
    let call = feedback
        .snapshot(68)
        .call_specialization_at(caller, call_pc)
        .unwrap();
    let clif = BaselineCompiler::host()
        .lower_with_direct_target_for_test(&verified, call_pc, call, 0x7654_3210)
        .expect("guarded baseline direct call");
    assert!(clif.contains("(i64, i32) -> i32"), "{clif}");
    assert!(clif.contains("0x1234_5678"), "{clif}");
    assert!(clif.contains("0x2234_5678"), "{clif}");
    assert!(clif.contains("0x7654_3210"), "{clif}");
    // The guarded miss remains connected to the generic CALL helper while
    // the matching edge invokes the scalar ABI directly.
    assert!(clif.matches("call_indirect").count() >= 3, "{clif}");
    assert!(clif.contains("brif"), "{clif}");
}

fn compile(
    bytecode: Vec<u8>,
    args: u16,
    locals: u16,
) -> rquickjs_jit::compiler::baseline::RelocatableCode {
    let function = verified_bytecode(bytecode, args, locals);
    compile_implemented_fixture(&BaselineCompiler::host(), &function).unwrap()
}

fn poll_sites(bytecode: Vec<u8>, args: u16, locals: u16) -> Vec<(u32, PollKind)> {
    let function = verified_bytecode(bytecode, args, locals);
    BaselineIr::translate_implemented_for_test(&function)
        .unwrap()
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction.op {
            IrOp::Poll { kind, .. } => Some((instruction.pc, kind)),
            _ => None,
        })
        .collect()
}

#[test]
fn baseline_ir_classifies_entry_loop_header_return_and_edge_polls() {
    let push_0 = named_opcode("push_0");
    let push_1 = named_opcode("push_1");
    let plus = named_opcode("plus");
    let put_loc0 = named_opcode("put_loc0");
    let put_loc1 = named_opcode("put_loc1");
    let get_loc0 = named_opcode("get_loc0");
    let get_loc1 = named_opcode("get_loc1");
    let get_arg0 = named_opcode("get_arg0");
    let lt = named_opcode("lt");
    let if_false8 = named_opcode("if_false8");
    let goto8 = named_opcode("goto8");
    let loop_sites = poll_sites(
        vec![
            push_0,
            plus,
            put_loc0,
            push_0,
            plus,
            put_loc1,
            get_loc1,
            get_arg0,
            lt,
            if_false8,
            11,
            get_loc0,
            get_loc1,
            opcode::ADD,
            put_loc0,
            get_loc1,
            push_1,
            opcode::ADD,
            put_loc1,
            goto8,
            (-14_i8) as u8,
            get_loc0,
            opcode::RETURN,
        ],
        1,
        2,
    );
    assert_eq!(loop_sites.first(), Some(&(0, PollKind::Entry)));
    assert_eq!(
        loop_sites
            .iter()
            .filter(|(_, kind)| *kind == PollKind::LoopHeader)
            .count(),
        1
    );
    assert_eq!(loop_sites.last(), Some(&(22, PollKind::Return)));

    let push_false = named_opcode("push_false");
    let edge_sites = poll_sites(
        vec![push_false, if_false8, 2, opcode::NOP, opcode::RETURN_UNDEF],
        0,
        0,
    );
    assert_eq!(
        edge_sites,
        vec![
            (0, PollKind::Entry),
            (3, PollKind::Edge),
            (4, PollKind::Return),
        ]
    );

    let mut periodic = vec![opcode::NOP; 1_025];
    periodic.push(opcode::RETURN_UNDEF);
    let periodic_sites = poll_sites(periodic, 0, 0);
    assert!(periodic_sites.contains(&(1_024, PollKind::Periodic)));
    assert_eq!(periodic_sites.last(), Some(&(1_025, PollKind::Return)));
}

fn assert_deep_retry(
    label: &str,
    executable: &rquickjs_jit::compiler::baseline::PublishedBaselineCode,
    mut frame: SyntheticFrame,
) {
    let before = frame.snapshot();

    let outcome = unsafe { frame.call(executable) };

    assert_eq!(
        outcome.exit.kind,
        qjs::JSJitExitKind_JS_JIT_EXIT_RETRY_INTERPRETER,
        "{label}"
    );
    assert_eq!(frame.snapshot(), before, "{label}");
}

fn assert_retry_predecessors_precede_the_first_poll(
    label: &str,
    code: &rquickjs_jit::compiler::baseline::RelocatableCode,
) {
    let lines: Vec<_> = code.clif().lines().collect();
    let mut current_block = None;
    let mut line_blocks = Vec::with_capacity(lines.len());
    let mut retry_candidates = Vec::new();
    let mut first_poll = None;

    for (line_index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("block") && trimmed.ends_with(':') {
            current_block = trimmed.split(['(', ':']).next().map(str::to_owned);
        }
        line_blocks.push(current_block.clone());
        if trimmed.contains("call_indirect") && first_poll.is_none() {
            first_poll = Some(line_index);
        }
        if trimmed.contains(&format!(
            "iconst.i32 {}",
            qjs::JSJitExitKind_JS_JIT_EXIT_RETRY_INTERPRETER
        )) {
            let value = trimmed
                .split_once(" = iconst.i32")
                .map(|(value, _)| value.trim().to_owned())
                .unwrap_or_else(|| panic!("{label}: malformed RETRY constant: {trimmed}"));
            retry_candidates.push((
                current_block
                    .clone()
                    .unwrap_or_else(|| panic!("{label}: RETRY outside a block: {trimmed}")),
                value,
            ));
        }
    }

    let retry_blocks: BTreeSet<_> = retry_candidates
        .into_iter()
        .filter_map(|(block, value)| {
            lines
                .iter()
                .zip(&line_blocks)
                .any(|(line, line_block)| {
                    line_block.as_ref() == Some(&block)
                        && line.trim().contains(&format!("store {value}, v0 "))
                })
                .then_some(block)
        })
        .collect();

    assert_eq!(retry_blocks.len(), 1, "{label}: {}", code.clif());
    let retry_block = retry_blocks.first().unwrap();
    if let Some(first_poll) = first_poll {
        for (line_index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let targets_retry = trimmed
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|token| token == retry_block);
            if trimmed.starts_with("brif") && targets_retry {
                assert!(
                    line_index < first_poll,
                    "{label}: RETRY predecessor after first poll\n{}",
                    code.clif()
                );
            }
        }
    } else {
        assert!(code.frame_states().is_empty(), "{label}: entry RETRY stub");
        assert!(
            !code.clif().lines().any(|line| line.contains(" load")),
            "{label}: entry RETRY stub read the frame\n{}",
            code.clif()
        );
    }
}

#[test]
fn machine_entry_executes_the_exact_aggregate_return_abi() {
    let executable = compile(vec![opcode::RETURN_UNDEF], 0, 0).publish().unwrap();
    let mut frame = SyntheticFrame::new(&[], 0, 0);
    assert_eq!(frame.stack_storage_address() & 15, 0);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
    assert_eq!(outcome.exit.reserved, 0);
    assert!(outcome.exit.resume_pc.is_null());
    assert!(outcome.exit.resume_stack_top.is_null());
    assert_eq!(outcome.result, JSValueRepr::undefined());
}

#[test]
fn cranelift_ir_has_hidden_sret_and_frame_indirect_poll() {
    let code = compile(vec![opcode::RETURN_UNDEF], 0, 0);
    let signature = code
        .clif()
        .lines()
        .find(|line| line.starts_with("function "))
        .expect("CLIF function signature");

    assert!(signature.contains("sret"), "{signature}");
    assert!(!signature.contains(" -> "), "{signature}");
    assert!(code.clif().contains("call_indirect"));
    assert!(code
        .relocations()
        .iter()
        .all(|relocation| !matches!(relocation.target, RelocationTarget::Symbol(_))));
}

#[test]
fn cranelift_output_retains_unwind_stack_and_frame_metadata() {
    let code = compile(vec![opcode::RETURN_UNDEF], 0, 0);

    assert!(code.unwind_metadata().is_some());
    assert_eq!(code.stack_maps().len(), code.frame_states().len());
    assert!(!code.frame_states().is_empty());
}

#[test]
fn published_clone_pins_metadata_and_deregisters_unwind_before_releasing_code() {
    let published = compile(vec![opcode::RETURN_UNDEF], 0, 0).publish().unwrap();
    let expected_states = published.frame_states().to_vec();
    let expected_maps = published.stack_maps().to_vec();
    let probe = published.lifetime_probe();
    let pin = published.clone();

    assert!(published.unwind_is_registered());
    drop(published);
    assert_eq!(pin.frame_states(), expected_states);
    assert_eq!(pin.stack_maps(), expected_maps);
    assert!(pin.unwind_metadata().is_some());
    assert!(probe.events().is_empty());

    let mut frame = SyntheticFrame::new(&[], 0, 0);
    assert_eq!(
        unsafe { frame.call(&pin) }.exit.kind,
        qjs::JSJitExitKind_JS_JIT_EXIT_DONE
    );
    drop(pin);

    assert_eq!(
        probe.events(),
        ["unwind_deregistered", "executable_released"]
    );
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[inline(never)]
fn invoke_with_unwind_probe(
    frame: &mut SyntheticFrame,
    executable: &rquickjs_jit::compiler::baseline::PublishedBaselineCode,
) -> rquickjs_jit::test_support::SyntheticOutcome {
    let outcome = unsafe { frame.call(executable) };
    std::hint::black_box(outcome.exit.kind);
    outcome
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn registered_eh_frame_unwinds_through_an_exact_generated_poll_pc() {
    let code = compile(vec![opcode::RETURN_UNDEF], 0, 0);
    let poll_offsets: BTreeSet<_> = code
        .frame_states()
        .iter()
        .filter(|state| state.location_kind == FrameStateLocationKind::CallReturn)
        .map(|state| state.code_offset as usize)
        .collect();
    let executable = code.publish().unwrap();
    let expected_poll_pcs: BTreeSet<_> = poll_offsets
        .iter()
        .map(|offset| executable.as_ptr() as usize + offset)
        .collect();
    let mut frame = SyntheticFrame::new(&[], 0, 0);
    frame.capture_backtrace_on_next_poll();

    let outcome = invoke_with_unwind_probe(&mut frame, &executable);

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
    let backtrace = frame.captured_backtrace();
    let jit_index = backtrace
        .iter()
        .position(|pc| expected_poll_pcs.contains(pc))
        .unwrap_or_else(|| {
            panic!(
                "backtrace did not contain an exact generated poll return PC; expected {expected_poll_pcs:x?}, got {backtrace:x?}"
            )
        });
    let rust_caller = invoke_with_unwind_probe as *const () as usize;
    assert!(
        backtrace[jit_index + 1..]
            .iter()
            .any(|pc| pc.abs_diff(rust_caller) < 4_096),
        "unwinding stopped at generated code; caller {rust_caller:#x}, backtrace {backtrace:x?}"
    );
}

#[test]
fn poll_states_use_exact_call_returns_and_non_call_states_use_exact_markers() {
    let code = compile(vec![opcode::RETURN_UNDEF], 0, 0);
    let states = code.frame_states();
    let same_pc_states: Vec<_> = states
        .iter()
        .filter(|state| state.bytecode_pc == 0)
        .collect();
    let distinct_offsets: BTreeSet<_> = states.iter().map(|state| state.code_offset).collect();

    assert!(
        same_pc_states.len() >= 3,
        "entry poll, return poll, and return state"
    );
    assert_eq!(distinct_offsets.len(), states.len(), "{states:?}");
    assert!(states
        .iter()
        .all(|state| (state.code_offset as usize) < code.bytes().len()));

    let call_returns: BTreeSet<_> = code.call_return_offsets().iter().copied().collect();
    let mut poll_count = 0;
    let mut marker_count = 0;
    for state in states {
        let source_loc = format!("@{:04x}", state.source_location);
        let source_lines: Vec<_> = code
            .clif()
            .lines()
            .filter(|line| line.contains(&source_loc))
            .collect();
        match state.location_kind {
            FrameStateLocationKind::CallReturn => {
                poll_count += 1;
                assert!(call_returns.contains(&state.code_offset), "{state:?}");
                assert!(state.source_start < state.code_offset, "{state:?}");
                assert!(state.code_offset <= state.source_end, "{state:?}");
                assert!(
                    source_lines
                        .iter()
                        .any(|line| line.contains("call_indirect")),
                    "{source_lines:?}"
                );
                assert!(
                    source_lines.iter().all(|line| !line.contains(" load")),
                    "poll source location leaked onto API loads: {source_lines:?}"
                );
            }
            FrameStateLocationKind::Marker => {
                marker_count += 1;
                assert_eq!(state.code_offset, state.source_start, "{state:?}");
                assert!(state.source_start < state.source_end, "{state:?}");
                assert!(
                    !call_returns.contains(&state.code_offset),
                    "marker aliased a call return: {state:?}"
                );
                assert!(
                    source_lines.iter().any(|line| line.contains("brif")),
                    "marker is not an emitted non-null frame branch: {source_lines:?}"
                );
            }
        }
    }
    assert!(poll_count >= 2, "entry and return polls");
    assert!(marker_count >= 1, "return exit marker");
}

#[test]
fn flattened_frame_slot_count_above_u16_max_is_rejected() {
    let function = verified_bytecode(vec![named_opcode("push_0"), opcode::RETURN], u16::MAX, 0);

    assert_eq!(
        BaselineIr::translate(&function),
        Err(CompileFailure::ResourceLimit)
    );
}

#[test]
fn baseline_rejects_ineligible_opcode_before_lowering() {
    let function = verified_bytecode(
        vec![
            opcode::PUSH_UNDEFINED,
            opcode::EVAL,
            0,
            0,
            0,
            0,
            opcode::RETURN,
        ],
        0,
        0,
    );
    assert_eq!(
        BaselineIr::translate(&function),
        Err(CompileFailure::Tier1Rejected(FallbackReason::DirectEval))
    );
}

#[test]
fn result_copy_preserves_all_sixteen_jsvalue_bytes() {
    let mut bytecode = vec![opcode::GET_ARG];
    bytecode.extend_from_slice(&0_u16.to_le_bytes());
    bytecode.push(opcode::RETURN);
    let executable = compile(bytecode, 1, 0).publish().unwrap();
    let sentinel = JSValueRepr::new(0x0123_4567_89ab_cdef, qjs::JS_TAG_FLOAT64 as i64);
    let mut frame = SyntheticFrame::new(&[sentinel], 0, 1);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
    assert_eq!(outcome.result, sentinel);
}

#[test]
fn dynamic_add_lowers_to_helpers_without_post_call_retry() {
    let mut bytecode = vec![opcode::GET_ARG];
    bytecode.extend_from_slice(&0_u16.to_le_bytes());
    bytecode.push(opcode::GET_ARG);
    bytecode.extend_from_slice(&1_u16.to_le_bytes());
    bytecode.extend([opcode::ADD, opcode::RETURN]);
    let code = compile(bytecode, 2, 0);

    assert_retry_predecessors_precede_the_first_poll("dynamic add helper", &code);
    assert!(code.call_return_offsets().len() >= 3, "{}", code.clif());
}

#[test]
fn checked_local_domain_failures_retry_before_poll_state_or_buffers_change() {
    let mut checked_local_bytecode = vec![named_opcode("get_loc_check")];
    checked_local_bytecode.extend_from_slice(&0_u16.to_le_bytes());
    checked_local_bytecode.push(opcode::RETURN);
    let checked_local = compile(checked_local_bytecode, 0, 1).publish().unwrap();
    let mut uninitialized = SyntheticFrame::new(&[], 1, 1);
    uninitialized.set_local(
        0,
        JSValueRepr::new(0xfeed_face_cafe_beef, qjs::JS_TAG_UNINITIALIZED as i64),
    );
    assert_deep_retry("uninitialized checked local", &checked_local, uninitialized);

    let mut explicitly_uninitialized_bytecode = vec![named_opcode("set_loc_uninitialized")];
    explicitly_uninitialized_bytecode.extend_from_slice(&0_u16.to_le_bytes());
    explicitly_uninitialized_bytecode.push(named_opcode("get_loc_check"));
    explicitly_uninitialized_bytecode.extend_from_slice(&0_u16.to_le_bytes());
    explicitly_uninitialized_bytecode.push(opcode::RETURN);
    let explicitly_uninitialized = compile(explicitly_uninitialized_bytecode, 0, 1)
        .publish()
        .unwrap();
    assert_deep_retry(
        "local explicitly becomes uninitialized",
        &explicitly_uninitialized,
        SyntheticFrame::new(&[], 1, 1),
    );
}

#[test]
fn every_generated_retry_predecessor_dominates_the_first_poll() {
    let return_undefined = compile(vec![opcode::RETURN_UNDEF], 1, 1);
    assert_retry_predecessors_precede_the_first_poll("frame guards", &return_undefined);

    let numeric = compile(
        vec![
            named_opcode("get_arg0"),
            named_opcode("push_1"),
            named_opcode("sub"),
            opcode::RETURN,
        ],
        1,
        0,
    );
    assert_retry_predecessors_precede_the_first_poll("numeric guards", &numeric);

    let static_failure = compile(
        vec![
            named_opcode("push_1"),
            named_opcode("push_1"),
            named_opcode("mod"),
            opcode::RETURN,
        ],
        0,
        0,
    );
    assert_retry_predecessors_precede_the_first_poll("entry retry stub", &static_failure);
}

#[test]
fn supported_immediate_truthiness_stays_native_and_exact() {
    let fixture =
        SnapshotFixture::compile("(function truthy(value) { if (value) return 1; return 0; })");
    let verified = fixture
        .snapshot()
        .verify(VerifyLimits::default())
        .expect("captured truthiness function verifies");
    let executable = BaselineCompiler::host()
        .compile(&verified)
        .expect("supported immediate truthiness compiles")
        .publish()
        .unwrap();

    for (value, expected) in [
        (JSValueRepr::int32(0), 0),
        (JSValueRepr::int32(-7), 1),
        (JSValueRepr::new(0, qjs::JS_TAG_BOOL as i64), 0),
        (JSValueRepr::new(1, qjs::JS_TAG_BOOL as i64), 1),
        (JSValueRepr::new(0, qjs::JS_TAG_NULL as i64), 0),
        (JSValueRepr::undefined(), 0),
        (JSValueRepr::new(0, qjs::JS_TAG_SHORT_BIG_INT as i64), 0),
        (JSValueRepr::new(7, qjs::JS_TAG_SHORT_BIG_INT as i64), 1),
        (JSValueRepr::float64(0.0), 0),
        (JSValueRepr::float64(-0.0), 0),
        (JSValueRepr::float64(f64::NAN), 0),
        (JSValueRepr::float64(1.5), 1),
    ] {
        let mut frame = SyntheticFrame::new(
            &[value],
            verified.snapshot().local_count() as usize,
            verified.snapshot().stack_size() as usize,
        );

        let outcome = unsafe { frame.call(&executable) };

        assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
        assert_eq!(outcome.result, JSValueRepr::int32(expected));
    }
}

#[test]
fn property_bytecode_lowers_to_borrowed_get_then_explicit_free() {
    let fixture = SnapshotFixture::compile("(function read(object) { return object.value; })");
    let verified = fixture
        .snapshot()
        .verify(VerifyLimits::default())
        .expect("captured property function verifies");

    let code = BaselineCompiler::host()
        .compile(&verified)
        .expect("property helpers compile");

    assert_retry_predecessors_precede_the_first_poll("property helpers", &code);
    assert!(code.call_return_offsets().len() >= 4, "{}", code.clif());
}

#[test]
fn every_helper_family_statically_fits_the_two_slot_scratch_tail() {
    let fixtures = [
        SnapshotFixture::compile("(function read(object) { return object.value; })"),
        SnapshotFixture::compile("(function invoke(fn, value) { fn(value); return 1; })"),
        SnapshotFixture::compile(
            "(function pair(value) { let copy = value; return [copy, copy]; })",
        ),
    ];
    let mut observed_two_slot_family = false;
    for fixture in &fixtures {
        let verified = fixture
            .snapshot()
            .verify(VerifyLimits::default())
            .expect("helper fixture verifies");
        let ir = BaselineIr::translate(&verified).expect("helper IR translates");
        let required = ir
            .max_stack_depth
            .saturating_sub(verified.snapshot().stack_size());
        assert!(
            required <= qjs::JS_JIT_HELPER_SCRATCH_SLOTS as u16,
            "required {required} scratch slots for {:?}",
            verified.snapshot().bytecode()
        );
        observed_two_slot_family |= required == 2;
        BaselineCompiler::host()
            .compile(&verified)
            .expect("statically bounded helper lowering compiles");
    }
    assert!(
        observed_two_slot_family,
        "fixture must exercise both scratch slots"
    );
}

#[test]
fn maximum_synthetic_logical_capacity_reserves_scratch_without_u16_overflow() {
    // Untrusted synthetic snapshots deliberately use u16::MAX as their
    // conservative logical stack capacity. Reserving the ABI scratch tail is
    // capacity arithmetic, not a reason to reject otherwise valid bytecode.
    let function = verified_bytecode(vec![opcode::RETURN_UNDEF], 0, 0);
    assert_eq!(function.snapshot().stack_size(), u16::MAX);

    compile_implemented_fixture(&BaselineCompiler::host(), &function)
        .expect("scratch capacity addition uses a widened integer");
}

#[test]
fn returning_a_borrowed_argument_duplicates_then_transfers_the_owned_result() {
    let mut bytecode = vec![opcode::GET_ARG];
    bytecode.extend_from_slice(&0_u16.to_le_bytes());
    bytecode.push(opcode::RETURN);
    let executable = compile(bytecode, 1, 0).publish().unwrap();
    let value = JSValueRepr::new(0x1111_2222_3333_4444, qjs::JS_TAG_FLOAT64 as i64);
    let mut frame = SyntheticFrame::new(&[value], 0, 1);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
    assert_eq!(outcome.result, value);
}

#[test]
fn overwrite_drop_dup_local_and_stack_preserve_owned_slot_transitions() {
    let paths = [
        compile(
            vec![
                named_opcode("get_arg0"),
                named_opcode("push_1"),
                named_opcode("put_arg0"),
                opcode::RETURN_UNDEF,
            ],
            1,
            0,
        ),
        compile(
            vec![
                named_opcode("get_arg0"),
                named_opcode("drop"),
                opcode::RETURN_UNDEF,
            ],
            1,
            0,
        ),
        compile(
            vec![
                named_opcode("get_arg0"),
                named_opcode("dup"),
                named_opcode("drop"),
                named_opcode("drop"),
                opcode::RETURN_UNDEF,
            ],
            1,
            0,
        ),
    ];

    for (index, code) in paths.iter().enumerate() {
        assert_retry_predecessors_precede_the_first_poll(&format!("owned path {index}"), code);
        assert!(code.call_return_offsets().len() >= 3, "{}", code.clif());
    }
}

#[test]
fn malformed_stack_ranges_retry_before_poll_or_dereference() {
    let executable = compile(vec![opcode::RETURN_UNDEF], 0, 0).publish().unwrap();

    for malformed in ["misaligned", "null", "oversize", "reversed", "wrapped"] {
        let mut frame = SyntheticFrame::new(&[], 0, 1);
        let base = frame.stack_storage_address();
        let (stack_base, stack_top) = match malformed {
            // This safe first case was the RED reproducer: the old equality-only
            // scan accepted it without dereferencing and reached the entry poll.
            "misaligned" => (base + 1, base + 1),
            "null" => (0, 0),
            "oversize" => (base, base + 16),
            "reversed" => (base + 16, base),
            "wrapped" => (usize::MAX & !15, 16),
            _ => unreachable!(),
        };
        frame.set_stack_bounds_raw(stack_base, stack_top);
        let before = frame.snapshot();

        let outcome = unsafe { frame.call(&executable) };

        assert_eq!(
            outcome.exit.kind,
            qjs::JSJitExitKind_JS_JIT_EXIT_RETRY_INTERPRETER,
            "{malformed} stack range"
        );
        assert_eq!(frame.snapshot(), before, "{malformed} stack range");
    }
}

#[test]
fn modulo_int32_fast_path_is_exact_and_only_non_negative_operands_stay_native() {
    // QuickJS's own `%` fast path: both Int32, dividend >= 0, divisor > 0.
    // Those results are exact `srem` values and can never be `-0`, trap on
    // `INT32_MIN % -1`, or divide by zero. Every other shape is routed to the
    // BINARY_ARITH_SLOW helper, so with the synthetic runtime's rejecting
    // helper table the body exits with an exception instead of computing a
    // wrong value natively.
    let executable = compile(
        vec![
            named_opcode("get_arg0"),
            named_opcode("get_arg1"),
            named_opcode("mod"),
            opcode::RETURN,
        ],
        2,
        0,
    )
    .publish()
    .unwrap();
    for (operands, expected) in [
        ([JSValueRepr::int32(7), JSValueRepr::int32(3)], 1),
        ([JSValueRepr::int32(0), JSValueRepr::int32(5)], 0),
        ([JSValueRepr::int32(2147483647), JSValueRepr::int32(2)], 1),
        ([JSValueRepr::int32(6), JSValueRepr::int32(2147483647)], 6),
    ] {
        let mut frame = SyntheticFrame::new(&operands, 0, 2);
        let outcome = unsafe { frame.call(&executable) };
        assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
        assert_eq!(outcome.result, JSValueRepr::int32(expected));
    }
    for operands in [
        [JSValueRepr::int32(5), JSValueRepr::float64(f64::INFINITY)],
        [JSValueRepr::int32(-4), JSValueRepr::int32(2)],
        [JSValueRepr::int32(4), JSValueRepr::int32(-2)],
        [JSValueRepr::int32(5), JSValueRepr::int32(0)],
        [JSValueRepr::int32(i32::MIN), JSValueRepr::int32(-1)],
        [JSValueRepr::float64(1e308), JSValueRepr::float64(1e-308)],
    ] {
        let mut frame = SyntheticFrame::new(&operands, 0, 2);
        let outcome = unsafe { frame.call(&executable) };
        assert_ne!(
            outcome.exit.kind,
            qjs::JSJitExitKind_JS_JIT_EXIT_DONE,
            "{operands:?} must not complete natively"
        );
    }
}

#[test]
fn interrupt_is_polled_immediately_before_return() {
    let executable = compile(vec![opcode::RETURN_UNDEF], 0, 0).publish().unwrap();
    let mut frame = SyntheticFrame::new(&[], 0, 0);
    frame.interrupt_on_poll(2);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_INTERRUPT);
    assert_eq!(frame.poll_count(), 2);
}

#[test]
fn loop_header_poll_uses_bounded_amortized_countdown_and_exact_resume_pc() {
    let push_0 = named_opcode("push_0");
    let push_1 = named_opcode("push_1");
    let plus = named_opcode("plus");
    let put_loc0 = named_opcode("put_loc0");
    let put_loc1 = named_opcode("put_loc1");
    let get_loc0 = named_opcode("get_loc0");
    let get_loc1 = named_opcode("get_loc1");
    let get_arg0 = named_opcode("get_arg0");
    let lt = named_opcode("lt");
    let if_false8 = named_opcode("if_false8");
    let goto8 = named_opcode("goto8");
    let bytecode = vec![
        push_0,
        plus,
        put_loc0,
        push_0,
        plus,
        put_loc1,
        get_loc1,
        get_arg0,
        lt,
        if_false8,
        11,
        get_loc0,
        get_loc1,
        opcode::ADD,
        put_loc0,
        get_loc1,
        push_1,
        opcode::ADD,
        put_loc1,
        goto8,
        (-14_i8) as u8,
        get_loc0,
        opcode::RETURN,
    ];
    let executable = compile(bytecode.clone(), 1, 2).publish().unwrap();
    // Exceed the production 1024-backedge cadence.  The second poll must be
    // the loop-header slow path rather than the mandatory return poll.
    let mut frame = SyntheticFrame::new(&[JSValueRepr::int32(1_100)], 2, 2);
    frame.set_bytecode(&bytecode);
    let bytecode_start = frame.bytecode_start();
    frame.interrupt_on_poll(2);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_INTERRUPT);
    assert_eq!(frame.poll_count(), 2);
    assert_eq!(
        unsafe { outcome.exit.resume_pc.offset_from(bytecode_start) },
        6,
        "the countdown slow path must resume at the loop header"
    );
}

#[test]
fn straight_line_code_polls_within_four_thousand_ninety_six_operations() {
    let mut bytecode = vec![opcode::NOP; 4_097];
    bytecode.push(opcode::RETURN_UNDEF);
    let executable = compile(bytecode.clone(), 0, 0).publish().unwrap();
    let mut frame = SyntheticFrame::new(&[], 0, 0);
    frame.set_bytecode(&bytecode);
    let bytecode_start = frame.bytecode_start();
    frame.interrupt_on_poll(2);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_INTERRUPT);
    assert_eq!(frame.poll_count(), 2);
    let resume_offset = unsafe { outcome.exit.resume_pc.offset_from(bytecode_start) };
    assert!(
        (0..=4_096).contains(&resume_offset),
        "second poll resumed at bytecode offset {resume_offset}"
    );
}

#[test]
fn forward_diamond_path_cannot_skip_lexically_inserted_polls() {
    let push_true = named_opcode("push_true");
    let push_false = named_opcode("push_false");
    let if_true8 = named_opcode("if_true8");
    let mut bytecode = Vec::new();
    let diamond_count = if cfg!(rquickjs_memory_sanitizer) {
        350
    } else {
        2_050
    };
    for _ in 0..diamond_count {
        bytecode.push(push_true);
        bytecode.push(if_true8);
        let forward_operand = bytecode.len();
        bytecode.push(0);

        let filler_start = bytecode.len();
        bytecode.push(opcode::NOP);
        bytecode.push(push_false);
        bytecode.push(if_true8);
        let backward_operand = bytecode.len();
        let backward = i8::try_from(filler_start as isize - backward_operand as isize).unwrap();
        bytecode.push(backward as u8);

        let next_diamond = bytecode.len();
        let forward = i8::try_from(next_diamond - forward_operand).unwrap();
        bytecode[forward_operand] = forward as u8;
    }
    bytecode.push(opcode::RETURN_UNDEF);

    let sites = poll_sites(bytecode.clone(), 0, 0);
    assert!(
        sites
            .iter()
            .filter(|(_, kind)| *kind == PollKind::Edge)
            .count()
            >= diamond_count - 50,
        "forward path must retain immediate edge polls"
    );

    let executable = compile(bytecode.clone(), 0, 0).publish().unwrap();
    let mut frame = SyntheticFrame::new(&[], 0, 0);
    frame.set_bytecode(&bytecode);
    let bytecode_start = frame.bytecode_start();
    frame.interrupt_on_poll(2);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_INTERRUPT);
    let resume_offset = unsafe { outcome.exit.resume_pc.offset_from(bytecode_start) };
    assert!(
        (0..4_096).contains(&resume_offset),
        "second poll was delayed until bytecode offset {resume_offset}"
    );
}

#[test]
fn cross_target_compilation_can_never_publish_on_the_host() {
    let target = if cfg!(target_arch = "x86_64") {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let flags = settings::Flags::new(settings::builder());
    let isa = isa::lookup(target.parse().unwrap())
        .unwrap()
        .finish(flags)
        .unwrap();
    let function = verified_bytecode(vec![opcode::RETURN_UNDEF], 0, 0);
    let code = compile_implemented_fixture(&BaselineCompiler::new(isa), &function).unwrap();

    assert!(matches!(
        code.publish(),
        Err(CodeMemoryError::TargetIsaMismatch)
    ));
}

#[test]
fn overflowing_add_uses_the_exact_add_helper_number_path() {
    let get_arg0 = named_opcode("get_arg0");
    let push_i32 = opcode::PUSH_I32;
    let add = opcode::ADD;
    let return_ = opcode::RETURN;

    let mut bytecode = vec![get_arg0, push_i32];
    bytecode.extend_from_slice(&1_i32.to_le_bytes());
    bytecode.extend([add, return_]);
    let executable = compile(bytecode, 1, 0).publish().unwrap();

    let mut ordinary = SyntheticFrame::new(&[JSValueRepr::int32(41)], 0, 2);
    let ordinary = unsafe { ordinary.call(&executable) };
    assert_eq!(ordinary.result, JSValueRepr::int32(42));

    let mut overflow = SyntheticFrame::new(&[JSValueRepr::int32(i32::MAX)], 0, 2);
    let overflow = unsafe { overflow.call(&executable) };
    assert_eq!(overflow.result.as_f64(), Some(i32::MAX as f64 + 1.0));
}

#[test]
fn compiles_loop_and_integer_arithmetic() {
    let push_0 = named_opcode("push_0");
    let push_1 = named_opcode("push_1");
    let plus = named_opcode("plus");
    let put_loc0 = named_opcode("put_loc0");
    let put_loc1 = named_opcode("put_loc1");
    let get_loc0 = named_opcode("get_loc0");
    let get_loc1 = named_opcode("get_loc1");
    let get_arg0 = named_opcode("get_arg0");
    let lt = named_opcode("lt");
    let if_false8 = named_opcode("if_false8");
    let goto8 = named_opcode("goto8");

    // s = 0; i = 0; while (i < n) { s += i; i += 1; } return s
    let bytecode = vec![
        push_0,
        plus,
        put_loc0,
        push_0,
        plus,
        put_loc1,
        get_loc1,
        get_arg0,
        lt,
        if_false8,
        11,
        get_loc0,
        get_loc1,
        opcode::ADD,
        put_loc0,
        get_loc1,
        push_1,
        opcode::ADD,
        put_loc1,
        goto8,
        (-14_i8) as u8,
        get_loc0,
        opcode::RETURN,
    ];
    let executable = compile(bytecode, 1, 2).publish().unwrap();
    let mut frame = SyntheticFrame::new(&[JSValueRepr::int32(100)], 2, 2);

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
    assert_eq!(outcome.result, JSValueRepr::int32(4_950));
    assert_eq!(
        frame.poll_count(),
        2,
        "entry + return; a sub-1024 loop must not cross the poll helper boundary"
    );
}

#[test]
fn floating_bit_coercion_unsigned_shift_and_negative_zero_match_quickjs_values() {
    let get_arg0 = named_opcode("get_arg0");
    let get_arg1 = named_opcode("get_arg1");

    let bit_not = compile(vec![get_arg0, named_opcode("not"), opcode::RETURN], 1, 0)
        .publish()
        .unwrap();
    let mut frame = SyntheticFrame::new(&[JSValueRepr::float64(1.5)], 0, 1);
    assert_eq!(
        unsafe { frame.call(&bit_not) }.result,
        JSValueRepr::int32(-2)
    );

    let unsigned_shift = compile(
        vec![get_arg0, get_arg1, named_opcode("shr"), opcode::RETURN],
        2,
        0,
    )
    .publish()
    .unwrap();
    let mut frame = SyntheticFrame::new(
        &[JSValueRepr::float64(4_294_967_295.0), JSValueRepr::int32(0)],
        0,
        2,
    );
    assert_eq!(
        unsafe { frame.call(&unsigned_shift) }.result,
        JSValueRepr::float64(4_294_967_295.0)
    );

    let multiply = compile(
        vec![get_arg0, get_arg1, named_opcode("mul"), opcode::RETURN],
        2,
        0,
    )
    .publish()
    .unwrap();
    let mut frame = SyntheticFrame::new(&[JSValueRepr::int32(0), JSValueRepr::int32(-1)], 0, 2);
    assert_eq!(
        unsafe { frame.call(&multiply) }.result,
        JSValueRepr::float64(-0.0)
    );
}

#[test]
fn compiles_quickjs_loop_snapshot_without_rewriting_its_opcodes() {
    let fixture = SnapshotFixture::compile(
        "(function sum(n, zero) { let s = zero; for (let i = zero; i < n; i++) s += i; return s; })",
    );
    let verified = fixture
        .snapshot()
        .verify(VerifyLimits::default())
        .expect("captured loop verifies");
    let names: Vec<_> = verified
        .instructions()
        .iter()
        .map(|instruction| instruction.opcode().name())
        .collect();
    let code = BaselineCompiler::host()
        .compile(&verified)
        .unwrap_or_else(|error| panic!("{error:?} lowering {names:?}"));
    if std::env::var_os("QJSJIT_DUMP_MACHINE").is_some() {
        eprintln!("{}", code.machine_disassembly());
    }
    if cfg!(target_arch = "x86_64") {
        let machine = code.machine_disassembly();
        let countdown = machine
            .find("$1024")
            .expect("machine loop contains the bounded poll countdown");
        let header_jump_line = machine[countdown..]
            .lines()
            .find(|line| line.trim_start().starts_with("jmp"))
            .expect("countdown preheader jumps to the machine loop header")
            .trim();
        let backedge_jump = machine
            .rfind(header_jump_line)
            .expect("numeric loop has a generated machine backedge");
        let block_start = machine[..backedge_jump]
            .rfind("block")
            .expect("machine backedge belongs to a block");
        let hot_backedge = &machine[block_start..backedge_jump];
        assert!(
            !hot_backedge.lines().any(|line| line.contains("call")),
            "hot backedge crossed a helper boundary:\n{hot_backedge}"
        );
        assert!(
            !hot_backedge.lines().any(|line| {
                line.contains(", ")
                    && ["(%rbx)", "(%r15)", "(%r14)"]
                        .iter()
                        .any(|frame_base| line.contains(frame_base))
            }),
            "hot backedge eagerly synchronized interpreter frame slots:\n{hot_backedge}"
        );
    }
    assert!(code
        .frame_states()
        .iter()
        .any(|state| state.code_offset > 0 && !state.slots.is_empty()));
    assert_eq!(code.stack_maps().len(), code.frame_states().len());
    assert!(code
        .stack_maps()
        .iter()
        .any(|map| map.code_offset > 0 && !map.live_slots.is_empty()));
    let executable = code.publish().unwrap();
    let mut frame = SyntheticFrame::new(
        &[JSValueRepr::int32(100), JSValueRepr::int32(0)],
        verified.snapshot().local_count() as usize,
        verified.snapshot().stack_size() as usize,
    );

    let outcome = unsafe { frame.call(&executable) };

    assert_eq!(outcome.exit.kind, qjs::JSJitExitKind_JS_JIT_EXIT_DONE);
    assert_eq!(outcome.result, JSValueRepr::int32(4_950));
}
