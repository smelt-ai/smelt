#![cfg(all(feature = "compiler", feature = "test-support"))]

use rquickjs_jit::bytecode::VerifyLimits;
use rquickjs_jit::ir::{
    HeapKey, OptimizedIr, OptimizedNodeKind, ScalarHeapEffect, ScalarHeapOperation, ScalarValue,
};
use rquickjs_jit::test_support::SnapshotFixture;

#[test]
fn method_property_load_retains_original_receiver_for_call() {
    let fixture = SnapshotFixture::compile("(function(object){return object.method()})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 601).unwrap();
    let graph = ir.scalar_graph();
    let load = ir
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(),
                OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "get_field2"
            )
        })
        .unwrap();
    let receiver = graph
        .inputs_for_block(0)
        .iter()
        .find(|(slot, _)| *slot == rquickjs_jit::ir::FrameSlot::Argument(0))
        .unwrap()
        .1;
    assert_eq!(graph.outputs_for_node(load.id())[0], receiver);
    let call = ir
        .nodes()
        .iter()
        .find_map(|node| graph.call(node.id()))
        .unwrap();
    assert_eq!(call.receiver, Some(receiver));
    assert_eq!(
        graph
            .frame_state_for_node(load.id())
            .unwrap()
            .stack
            .as_ref(),
        &[receiver]
    );
}

#[test]
fn heap_accesses_preserve_pre_effect_operands_including_zero_result_stores() {
    for source in [
        "(function(object,value){object.x=value;return object.x})",
        "(function(object,index,value){object[index]=value;return object[index]})",
        "(function(object,index,value){return object[index]=value})",
        "(function(object,value){return object.x=value})",
    ] {
        let fixture = SnapshotFixture::compile(source);
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let ir = OptimizedIr::translate(&verified, 602).unwrap();
        let graph = ir.scalar_graph();
        let mut stores = 0;
        for node in ir.nodes() {
            let Some(access) = graph.heap_operation(node.id()) else {
                continue;
            };
            let state = graph
                .frame_state_for_node(access.frame_state_node())
                .unwrap();
            assert_eq!(state.pc, node.pc());
            let operands = &state.stack[state.stack.len() - usize::from(node.pops())..];
            assert_eq!(
                graph.effect_for_node(node.id()),
                ScalarHeapEffect::Reentrant
            );
            match *access {
                ScalarHeapOperation::GetProperty {
                    base, atom, result, ..
                } => {
                    assert_eq!(operands, &[base]);
                    assert!(
                        matches!(graph.values()[result.index()], ScalarValue::GetProperty {base: b, atom: a, ..} if b == base && a == atom)
                    );
                }
                ScalarHeapOperation::GetElement {
                    base,
                    index,
                    result,
                    ..
                } => {
                    assert_eq!(operands, &[base, index]);
                    assert!(
                        matches!(graph.values()[result.index()], ScalarValue::GetElement {base: b, index: i, ..} if b == base && i == index)
                    );
                }
                ScalarHeapOperation::PutProperty { base, value, .. } => {
                    stores += 1;
                    assert_eq!(operands, &[base, value]);
                    assert!(graph.outputs_for_node(node.id()).is_empty());
                    assert!(graph.value_for_node(node.id()).is_none());
                }
                ScalarHeapOperation::PutElement {
                    base, index, value, ..
                } => {
                    stores += 1;
                    assert_eq!(operands, &[base, index, value]);
                    assert!(graph.outputs_for_node(node.id()).is_empty());
                }
            }
        }
        assert_eq!(stores, 1, "{source}");
    }
}

#[test]
fn guarded_data_locations_only_disambiguate_distinct_named_properties() {
    let fixture = SnapshotFixture::compile("(function(a,b,i,j){a.x=1;b.x=2;b.y=3;a[i]=4;a[j]=5})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 603).unwrap();
    let operations: Vec<_> = ir
        .nodes()
        .iter()
        .filter_map(|node| ir.scalar_graph().heap_operation(node.id()))
        .collect();
    assert_eq!(operations.len(), 5);
    let locations: Vec<_> = operations
        .iter()
        .map(|operation| operation.location())
        .collect();
    assert!(locations[0].may_alias(locations[1]));
    assert!(!locations[0].may_alias(locations[2]));
    assert!(locations[0].may_alias(locations[3]));
    assert!(locations[3].may_alias(locations[4]));
    assert!(matches!(locations[3].key, HeapKey::Element(_)));
    assert_eq!(
        operations[0].guarded_effect(),
        ScalarHeapEffect::Write(locations[0])
    );
    assert!(ScalarHeapEffect::Reentrant.invalidates(locations[0]));
    assert!(ScalarHeapEffect::Safepoint.invalidates(locations[0]));
    assert!(!operations[2].guarded_effect().invalidates(locations[0]));
}

#[test]
fn unsupported_computed_method_access_fails_closed() {
    let fixture = SnapshotFixture::compile("(function(object,key){return object[key]()})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    assert!(matches!(
        OptimizedIr::translate(&verified, 604),
        Err(rquickjs_jit::compiler::CompileFailure::UnsupportedOpcode)
    ));
}

#[test]
fn property_assignment_permutation_keeps_return_value_and_store_operands() {
    use rquickjs_jit::ir::FrameSlot;
    let fixture = SnapshotFixture::compile("(function(object,value){return object.x=value})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 606).unwrap();
    let graph = ir.scalar_graph();
    let argument = |index| {
        graph
            .inputs_for_block(0)
            .iter()
            .find(|(slot, _)| *slot == FrameSlot::Argument(index))
            .unwrap()
            .1
    };
    let store = ir
        .nodes()
        .iter()
        .find_map(|node| graph.heap_operation(node.id()))
        .unwrap();
    let ScalarHeapOperation::PutProperty {
        base,
        value,
        frame_state_node,
        ..
    } = *store
    else {
        panic!("missing property store")
    };
    assert_eq!(base, argument(0));
    assert_eq!(value, argument(1));
    assert_eq!(
        graph
            .frame_state_for_node(frame_state_node)
            .unwrap()
            .stack
            .as_ref(),
        &[argument(1), argument(0), argument(1)]
    );
}

#[test]
fn calls_polls_and_frame_writes_are_observable_effect_boundaries() {
    use rquickjs_jit::ir::FrameSlot;
    let fixture = SnapshotFixture::compile(
        "(function(object,f,n){let x=0;for(let i=0;i<n;i++){x=object.x;f()}return x})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 607).unwrap();
    let graph = ir.scalar_graph();
    let mut calls = 0;
    let mut polls = 0;
    let mut writes = 0;
    for node in ir.nodes() {
        if graph.call(node.id()).is_some() {
            calls += 1;
            assert_eq!(
                graph.effect_for_node(node.id()),
                ScalarHeapEffect::Reentrant
            );
            assert!(graph.frame_state_for_node(node.id()).is_some());
        }
        if matches!(
            node.kind(),
            OptimizedNodeKind::GuardNumeric { mid_loop: true, .. }
        ) {
            polls += 1;
            assert_eq!(
                graph.effect_for_node(node.id()),
                ScalarHeapEffect::Safepoint
            );
        }
        if matches!(
            graph.effect_for_node(node.id()),
            ScalarHeapEffect::FrameWrite(FrameSlot::Local(_))
        ) {
            writes += 1;
            assert!(graph.effect_for_node(node.id()).observes_heap());
        }
    }
    assert_eq!(calls, 1);
    assert!(polls > 0);
    assert!(writes > 0);
}

#[test]
fn special_length_opcode_is_an_explicit_generic_property_read() {
    let fixture = SnapshotFixture::compile("(function(object){return object.length})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let ir = OptimizedIr::translate(&verified, 608).unwrap();
    let graph = ir.scalar_graph();
    let node = ir
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(),
                OptimizedNodeKind::Bytecode { opcode } if opcode.as_ref() == "get_length"
            )
        })
        .unwrap();
    assert!(
        matches!(graph.heap_operation(node.id()), Some(ScalarHeapOperation::GetProperty { atom, .. }) if *atom == rquickjs::qjs::JS_ATOM_length)
    );
    assert_eq!(
        graph.effect_for_node(node.id()),
        ScalarHeapEffect::Reentrant
    );
    assert!(graph.frame_state_for_node(node.id()).is_some());
}
