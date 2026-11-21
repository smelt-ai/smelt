#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
use rquickjs_jit::{
    bytecode::VerifyLimits,
    compiler::optimized::Tier2Compiler,
    runtime::{
        FeedbackSnapshot, FunctionKey, ObservedType, PropertyAttributes, PrototypeDependencyToken,
        ShapeFeedbackTable, ShapeObservation, ShapeToken,
    },
    test_support::SnapshotFixture,
};

#[test]
fn production_property_specialization_survives_shape_mutation_and_stress_gc() {
    use rquickjs::{Context, Function, Object, Runtime};
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
    context
        .with(|ctx| {
            ctx.eval::<(), _>("globalThis.shared={answer:41};function f(o){return o.answer}")
        })
        .unwrap();
    for _ in 0..128 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("f").unwrap();
            let o: Object = ctx.globals().get("shared").unwrap();
            assert_eq!(f.call::<_, i32>((o,)).unwrap(), 41);
        });
        jit.poll();
        std::thread::yield_now();
    }
    for _ in 0..10_000 {
        jit.poll();
        if jit.metrics().installed >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    context.with(|ctx| {
        let f: Function = ctx.globals().get("f").unwrap();
        let o: Object = ctx.globals().get("shared").unwrap();
        assert_eq!(f.call::<_, i32>((o,)).unwrap(), 41);
    });
    jit.poll();
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
    let before = jit.metrics();
    context
        .with(|ctx| ctx.eval::<(), _>("shared.extra=1;shared.answer=42"))
        .unwrap();
    for _ in 0..1_024 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("f").unwrap();
            let o: Object = ctx.globals().get("shared").unwrap();
            assert_eq!(f.call::<_, i32>((o,)).unwrap(), 42);
        });
        jit.poll();
    }
    assert!(jit.metrics().deopts > before.deopts, "{:?}", jit.metrics());
}

#[test]
fn production_bounded_polymorphic_property_hits_each_layout_without_deopt() {
    use rquickjs::{Context, Function, Object, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
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
            ctx.eval::<(), _>(
                "globalThis.left={answer:41};\
                 globalThis.right={padding:1,answer:42};\
                 function poly(o){return o.answer}",
            )
        })
        .unwrap();
    // First install only the baseline. Property feedback is emitted by that
    // native tier, so collect both layouts before the next maintenance poll is
    // allowed to snapshot and queue Tier2.
    for index in 0..10_000 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("poly").unwrap();
            let o: Object = ctx
                .globals()
                .get(if index & 1 == 0 { "left" } else { "right" })
                .unwrap();
            let result = f.call::<_, i32>((o,));
            assert!(result.is_ok(), "{result:?}; catch={:?}", ctx.catch());
            assert_eq!(result.unwrap(), 41 + (index & 1));
        });
        jit.poll();
        if jit.metrics().installed > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().installed > 0, "{:?}", jit.metrics());
    assert_eq!(jit.metrics().tier2_entries, 0, "{:?}", jit.metrics());
    for index in 0..128 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("poly").unwrap();
            let o: Object = ctx
                .globals()
                .get(if index & 1 == 0 { "left" } else { "right" })
                .unwrap();
            let result = f.call::<_, i32>((o,));
            assert!(result.is_ok(), "{result:?}; catch={:?}", ctx.catch());
            assert_eq!(result.unwrap(), 41 + (index & 1));
        });
    }
    for index in 0..10_000 {
        jit.poll();
        context.with(|ctx| {
            let f: Function = ctx.globals().get("poly").unwrap();
            let o: Object = ctx
                .globals()
                .get(if index & 1 == 0 { "left" } else { "right" })
                .unwrap();
            let result = f.call::<_, i32>((o,));
            assert!(result.is_ok(), "{result:?}; catch={:?}", ctx.catch());
            assert_eq!(result.unwrap(), 41 + (index & 1));
        });
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    unsafe extern "C" {
        fn JS_JitGetHelperCount(
            rt: *mut rquickjs_core::qjs::JSRuntime,
            helper: u32,
            count: *mut u64,
        ) -> i32;
    }
    let rt =
        context.with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
    let shape_calls = || {
        let mut count = 0;
        assert_eq!(
            unsafe {
                JS_JitGetHelperCount(
                    rt,
                    rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SHAPE_GUARD,
                    &mut count,
                )
            },
            0
        );
        count
    };
    let before_shape_calls = shape_calls();
    let before = jit.metrics();
    assert!(before.tier2_entries > 0, "{before:?}");
    for index in 0..1_024 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("poly").unwrap();
            let o: Object = ctx
                .globals()
                .get(if index & 1 == 0 { "left" } else { "right" })
                .unwrap();
            assert_eq!(f.call::<_, i32>((o,)).unwrap(), 41 + (index & 1));
        });
        jit.poll();
    }
    assert_eq!(
        shape_calls(),
        before_shape_calls,
        "native property hits must not cross the shape helper"
    );
    let after = jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries, "{after:?}");
    assert_eq!(after.deopts, before.deopts, "{before:?} -> {after:?}");
}

#[test]
fn guarded_own_primitive_property_lowers_with_owned_deopt_bridge() {
    let fixture = SnapshotFixture::compile("(function(o){return o.answer})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let pc = verified
        .instructions()
        .iter()
        .find(|i| i.opcode().name() == "get_field")
        .unwrap()
        .pc();
    let key = FunctionKey::new(1, 1);
    let mut table = ShapeFeedbackTable::new(3);
    table.observe(
        key,
        pc,
        ShapeObservation::new(
            ShapeToken::new(0x1000, 7),
            PrototypeDependencyToken::new(0, 0),
            1,
            PropertyAttributes::WRITABLE,
            ObservedType::Int32,
        ),
    );
    let feedback = FeedbackSnapshot::empty(1).with_properties(table.snapshot(key));
    let clif = Tier2Compiler::host(1)
        .lower_with_feedback_for_test(&verified, key, &feedback)
        .expect("owned property bridge");
    // Block layout may place the owning deopt bridge before the successful
    // continuation. Check reachable native-return paths, not textual order.
    assert_eq!(
        guarded_property_hit_is_leaf(&clif, 0x1000),
        Ok(()),
        "{clif}"
    );

    // Prove that the structural assertion rejects real hot-path regressions:
    // moving either the existing owner helper or root spill into the entry
    // path must fail, even though a successful return remains reachable.
    let branch = clif
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("brif "))
        .unwrap();
    let helper = clif
        .lines()
        .map(str::trim)
        .find(|line| line.contains("call_indirect"))
        .unwrap();
    let with_hot_helper = clif.replacen(branch, &format!("{helper}\n    {branch}"), 1);
    assert!(guarded_property_hit_is_leaf(&with_hot_helper, 0x1000)
        .unwrap_err()
        .contains("helper"));
    let frame = clif
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("block0("))
        .unwrap()
        .split(',')
        .nth(1)
        .unwrap()
        .trim()
        .split(':')
        .next()
        .unwrap();
    let argument_address = format!(
        "{frame}+{}",
        core::mem::offset_of!(rquickjs::qjs::JSJitExecFrame, arg_buf)
    );
    let argument_buffer = clif
        .lines()
        .map(str::trim)
        .find_map(|line| {
            let (value, operation) = line.split_once(" = ")?;
            (operation.starts_with("load") && operation.ends_with(&argument_address))
                .then_some(value)
        })
        .unwrap();
    let spill = clif
        .lines()
        .map(|line| line.split(';').next().unwrap().trim())
        .find(|line| line.starts_with("store") && line.ends_with(&format!(", {argument_buffer}")))
        .unwrap();
    let with_hot_spill = clif.replacen(branch, &format!("{spill}\n    {branch}"), 1);
    assert!(guarded_property_hit_is_leaf(&with_hot_spill, 0x1000)
        .unwrap_err()
        .contains("root spill"));
}

fn guarded_property_hit_is_leaf(clif: &str, shape: i64) -> Result<(), String> {
    guarded_property_path(clif, shape, None)
}

fn guarded_property_path(clif: &str, shape: i64, store_offset: Option<i32>) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet};

    fn immediate(text: &str) -> Option<i64> {
        let text = text.replace('_', "");
        text.strip_prefix("0x").map_or_else(
            || text.parse().ok(),
            |hex| i64::from_str_radix(hex, 16).ok(),
        )
    }
    fn constant(value: &str, definitions: &BTreeMap<&str, &str>, remaining: usize) -> Option<i64> {
        if remaining == 0 {
            return None;
        }
        let definition = definitions.get(value)?;
        if definition.starts_with('v') && !definition.contains(' ') {
            return constant(definition, definitions, remaining - 1);
        }
        let (operation, operands) = definition.split_once(' ')?;
        match operation.split('.').next()? {
            "iconst" => immediate(operands),
            "band" | "bor" => {
                let (a, b) = operands.split_once(", ")?;
                let a = constant(a, definitions, remaining - 1)?;
                let b = constant(b, definitions, remaining - 1)?;
                Some(if operation.starts_with("band") {
                    a & b
                } else {
                    a | b
                })
            }
            _ => None,
        }
    }
    fn source<'a>(mut value: &'a str, definitions: &BTreeMap<&str, &'a str>) -> &'a str {
        for _ in 0..definitions.len() {
            let Some(&definition) = definitions.get(value) else {
                break;
            };
            if definition.starts_with('v') && !definition.contains(' ') {
                value = definition;
            } else if definition.starts_with("iadd_imm") {
                value = definition
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .trim_end_matches(',');
            } else {
                break;
            }
        }
        value
    }
    fn store(line: &str) -> Option<(&str, &str)> {
        let (operation, operands) = line.split_once(' ')?;
        operation.starts_with("store").then_some(())?;
        operands.split_once(", ")
    }
    fn base(address: &str) -> &str {
        address.split(['+', '-']).next().unwrap()
    }
    fn label(text: &str) -> &str {
        text.split(['(', ':', ',', ' ']).next().unwrap()
    }
    fn success_target<'a>(lines: &[&'a str]) -> Option<&'a str> {
        let branch = lines.iter().find(|line| line.starts_with("brif"))?;
        Some(label(&branch[branch.find("block")?..]))
    }
    fn guards_are_conjoined(
        lines: &[&str],
        definitions: &BTreeMap<&str, &str>,
        checks: &[&str],
    ) -> bool {
        let Some(branch) = lines.iter().find(|line| line.starts_with("brif")) else {
            return false;
        };
        let predicate = branch
            .split_whitespace()
            .nth(1)
            .unwrap()
            .trim_end_matches(',');
        definitions
            .get(predicate)
            .and_then(|operation| operation.strip_prefix("band "))
            .and_then(|operands| operands.split_once(", "))
            .is_some_and(|(a, b)| {
                checks.len() == 2 && checks.contains(&a) && checks.contains(&b) && a != b
            })
    }

    let mut definitions = BTreeMap::new();
    let mut blocks = BTreeMap::<&str, Vec<&str>>::new();
    let mut entry = None;
    let mut parameters = Vec::new();
    let mut current = None;
    for line in clif
        .lines()
        .map(|line| line.split(';').next().unwrap().trim())
    {
        if line.starts_with("block") {
            let id = label(line);
            if entry.is_none() {
                entry = Some(id);
                parameters = line
                    .split_once('(')
                    .unwrap()
                    .1
                    .split(')')
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|p| p.trim().split(':').next().unwrap())
                    .collect();
            }
            current = Some(id);
            blocks.entry(id).or_default();
        } else if let Some(id) = current {
            blocks.get_mut(id).unwrap().push(line);
            if let Some((value, definition)) =
                line.split_once(" = ").or_else(|| line.split_once(" -> "))
            {
                definitions.insert(value, definition);
            }
        }
    }
    let entry = entry.ok_or("missing entry block")?;
    let sret = *parameters.first().ok_or("missing result parameter")?;
    let frame = *parameters.get(1).ok_or("missing frame parameter")?;
    let root_loads = [
        format!(
            "{frame}+{}",
            core::mem::offset_of!(rquickjs::qjs::JSJitExecFrame, arg_buf)
        ),
        format!(
            "{frame}+{}",
            core::mem::offset_of!(rquickjs::qjs::JSJitExecFrame, var_buf)
        ),
    ];
    let roots: BTreeSet<_> = definitions
        .iter()
        .filter_map(|(&value, &definition)| {
            (definition.starts_with("load")
                && root_loads
                    .iter()
                    .any(|address| definition.ends_with(address)))
            .then_some(value)
        })
        .collect();
    // Cold owner-materialization paths may reload a root buffer pointer from
    // the frame (inline `js_dup` of an argument/local), so every such load is
    // a root; both buffers must be present.
    if !root_loads.iter().all(|address| {
        definitions
            .values()
            .any(|definition| definition.starts_with("load") && definition.ends_with(address))
    }) {
        return Err("missing argument/local root buffers".into());
    }
    let mut edges = BTreeMap::<&str, Vec<&str>>::new();
    let mut done = BTreeSet::new();
    for (&id, lines) in &blocks {
        if lines.contains(&"return")
            && lines.iter().any(|line| {
                store(line).is_some_and(|(value, destination)| {
                    destination == sret
                        && constant(value, &definitions, definitions.len())
                            == Some(i64::from(rquickjs::qjs::JSJitExitKind_JS_JIT_EXIT_DONE))
                })
            })
        {
            done.insert(id);
        }
        let outgoing = edges.entry(id).or_default();
        for line in lines
            .iter()
            .filter(|line| line.starts_with("brif") || line.starts_with("jump "))
        {
            let selected = if line.starts_with("brif") {
                let condition = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .trim_end_matches(',');
                constant(condition, &definitions, definitions.len()).map(|v| usize::from(v == 0))
            } else {
                None
            };
            for (edge, target) in line.match_indices("block").enumerate() {
                if selected.is_none_or(|selected| selected == edge) {
                    outgoing.push(label(&line[target.0..]));
                }
            }
        }
    }
    if done.is_empty() {
        return Err("missing normal return".into());
    }
    if let Some(offset) = store_offset {
        let (&shape_block, shape_lines) = blocks
            .iter()
            .find(|(_, lines)| {
                lines.iter().any(|line| {
                    line.contains("icmp_imm")
                        && line.contains(" eq ")
                        && line.rsplit_once(", ").and_then(|(_, v)| immediate(v)) == Some(shape)
                })
            })
            .ok_or("missing polymorphic shape guard")?;
        let shape_checks: Vec<_> = shape_lines
            .iter()
            .filter_map(|line| {
                let (result, operation) = line.split_once(" = ")?;
                let value = operation
                    .rsplit_once(", ")
                    .and_then(|(_, v)| immediate(v))?;
                (operation.starts_with("icmp_imm")
                    && operation.contains(" eq ")
                    && (value == shape || value == 7))
                    .then_some(result)
            })
            .collect();
        if !guards_are_conjoined(shape_lines, &definitions, &shape_checks) {
            return Err("shape identity and live generation must both guard the field".into());
        }
        let field_block = success_target(shape_lines).ok_or("missing shape success edge")?;
        let field_lines = &blocks[field_block];
        let store_block = success_target(field_lines).ok_or("missing tag success edge")?;
        let stores: Vec<_> = blocks[store_block]
            .iter()
            .filter_map(|line| store(line))
            .collect();
        if stores.len() != 2 {
            return Err("successful property store must write both JSValue words".into());
        }
        let properties = base(stores[0].1);
        let address = |offset| {
            if offset == 0 {
                properties.to_owned()
            } else {
                format!("{properties}+{offset}")
            }
        };
        if stores[0].1 != address(offset) || stores[1].1 != address(offset + 8) {
            return Err("wrong guarded property offset".into());
        }
        // Both the old field and replacement must be primitive before the raw
        // write; checking only one side would violate the ownership contract.
        let comparisons: Vec<_> = field_lines
            .iter()
            .filter_map(|line| {
                let (result, operation) = line.split_once(" = ")?;
                if !operation.starts_with("icmp_imm")
                    || !operation.contains(" eq ")
                    || operation.rsplit_once(", ").and_then(|(_, v)| immediate(v)) != Some(0)
                {
                    return None;
                }
                let operand = operation.split_whitespace().nth(2)?.trim_end_matches(',');
                Some((result, source(operand, &definitions)))
            })
            .collect();
        let current_tag = comparisons.iter().any(|(_, operand)| {
            definitions.get(operand).is_some_and(|operation| {
                operation.starts_with("load") && operation.ends_with(&address(offset + 8))
            })
        });
        let input_tag = comparisons
            .iter()
            .any(|(_, operand)| *operand == source(stores[1].0, &definitions));
        let tag_checks: Vec<_> = comparisons.iter().map(|(id, _)| *id).collect();
        let both = guards_are_conjoined(field_lines, &definitions, &tag_checks);
        if !current_tag || !input_tag || !both {
            return Err("raw store is not guarded by both primitive tags".into());
        }
        if !edges[shape_block].contains(&field_block) || !edges[field_block].contains(&store_block)
        {
            return Err("missing native property store path".into());
        }
        // Stop at the semantic store. Returning the unrelated borrowed value
        // has a separate stress-GC/ownership branch after this native region.
        done = BTreeSet::from([store_block]);
    }
    let mut reaches_done = done;
    loop {
        let previous = reaches_done.len();
        for (&id, outgoing) in &edges {
            if outgoing.iter().any(|target| reaches_done.contains(target)) {
                reaches_done.insert(id);
            }
        }
        if previous == reaches_done.len() {
            break;
        }
    }
    let mut pending = vec![entry];
    let mut visited = BTreeSet::new();
    let mut checked_shape = false;
    let mut checked_field_tag = false;
    while let Some(id) = pending.pop() {
        if !reaches_done.contains(id) || !visited.insert(id) {
            continue;
        }
        let lines = blocks.get(id).ok_or("unknown branch target")?;
        for line in lines {
            if line.contains("call_indirect") || line.contains(" = call ") {
                return Err(format!("helper on successful path in {id}"));
            }
            checked_shape |= line.contains("icmp_imm")
                && line.contains(" eq ")
                && line.rsplit_once(", ").and_then(|(_, v)| immediate(v)) == Some(shape);
            if let Some((_, operation)) = line.split_once(" = ") {
                if operation.starts_with("icmp_imm")
                    && operation.contains(" eq ")
                    && operation.rsplit_once(", ").and_then(|(_, v)| immediate(v))
                        == Some(i64::from(rquickjs::qjs::JS_TAG_INT))
                {
                    let operand = operation
                        .split_whitespace()
                        .nth(2)
                        .unwrap()
                        .trim_end_matches(',');
                    checked_field_tag |= definitions
                        .get(source(operand, &definitions))
                        .is_some_and(|definition| {
                            definition.starts_with("load")
                                && definition.split_whitespace().last().is_some_and(|address| {
                                    !roots.contains(source(base(address), &definitions))
                                })
                        });
                }
            }
            if let Some((value, destination)) = store(line) {
                let value = source(value, &definitions);
                let root_value = definitions.get(value).is_some_and(|definition| {
                    definition.starts_with("load")
                        && definition.split_whitespace().last().is_some_and(|address| {
                            roots.contains(source(base(address), &definitions))
                        })
                });
                let stack_spill = store_offset.is_some()
                    && definitions
                        .get(source(base(destination), &definitions))
                        .is_some_and(|operation| {
                            operation.starts_with("load")
                                && operation.ends_with(&format!(
                                    "{frame}+{}",
                                    core::mem::offset_of!(
                                        rquickjs::qjs::JSJitExecFrame,
                                        stack_base
                                    )
                                ))
                        });
                if (root_value && store_offset.is_none())
                    || stack_spill
                    || roots.contains(source(base(destination), &definitions))
                {
                    return Err(format!("root spill on successful path in {id}: {line}"));
                }
            }
        }
        pending.extend(edges[id].iter().copied());
    }
    if !checked_shape {
        return Err("no guarded shape path reaches normal return".into());
    }
    if !checked_field_tag {
        return Err("no guarded primitive field reaches normal return".into());
    }
    Ok(())
}

#[test]
fn bounded_polymorphic_property_guards_do_not_add_helper_calls() {
    let fixture = SnapshotFixture::compile("(function(o){return o.answer})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let pc = verified
        .instructions()
        .iter()
        .find(|i| i.opcode().name() == "get_field")
        .unwrap()
        .pc();
    let key = FunctionKey::new(1, 1);
    let compile = |observations: &[(u64, u32)]| {
        let mut table = ShapeFeedbackTable::new(3);
        for &(identity, offset) in observations {
            table.observe(
                key,
                pc,
                ShapeObservation::new(
                    ShapeToken::new(identity, 7),
                    PrototypeDependencyToken::new(0, 0),
                    offset,
                    PropertyAttributes::WRITABLE,
                    ObservedType::Int32,
                ),
            );
        }
        Tier2Compiler::host(1)
            .lower_with_feedback_for_test(
                &verified,
                key,
                &FeedbackSnapshot::empty(1).with_properties(table.snapshot(key)),
            )
            .unwrap()
    };
    let monomorphic = compile(&[(0x1000, 1)]);
    let polymorphic = compile(&[(0x1000, 1), (0x2000, 3), (0x3000, 2)]);
    assert_eq!(
        polymorphic.matches("call_indirect").count(),
        monomorphic.matches("call_indirect").count(),
        "{polymorphic}"
    );
    assert!(polymorphic.contains(", 8192"), "{polymorphic}");
    assert!(polymorphic.contains(", 0x3000"), "{polymorphic}");
}

#[test]
fn bounded_polymorphic_primitive_store_emits_a_guard_chain_and_raw_stores() {
    let fixture = SnapshotFixture::compile("(function(o,v){o.answer=v;return v})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let pc = verified
        .instructions()
        .iter()
        .find(|i| i.opcode().name() == "put_field")
        .unwrap()
        .pc();
    let key = FunctionKey::new(1, 1);
    let mut table = ShapeFeedbackTable::new(3);
    for (identity, offset) in [(0x1000, 0), (0x2000, 2)] {
        table.observe(
            key,
            pc,
            ShapeObservation::new(
                ShapeToken::new(identity, 7),
                PrototypeDependencyToken::new(0, 0),
                offset,
                PropertyAttributes::WRITABLE,
                ObservedType::Int32,
            ),
        );
    }
    let clif = Tier2Compiler::host(1)
        .lower_with_feedback_for_test(
            &verified,
            key,
            &FeedbackSnapshot::empty(1).with_properties(table.snapshot(key)),
        )
        .expect("bounded primitive store PIC");
    // Two deopt owner calls and a separate return-value ownership/stress path
    // may exist. Neither shape's guarded native store may execute those calls.
    for (shape, offset) in [(0x1000, 0), (0x2000, 32)] {
        assert_eq!(
            guarded_property_path(&clif, shape, Some(offset)),
            Ok(()),
            "{clif}"
        );
        assert!(
            guarded_property_path(&clif, shape, Some(offset + 16)).is_err(),
            "the assertion must reject a wrong field offset"
        );
    }
    let branch = clif
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("brif "))
        .unwrap();
    let helper = clif
        .lines()
        .map(str::trim)
        .find(|line| line.contains("call_indirect"))
        .unwrap();
    let with_hot_helper = clif.replacen(branch, &format!("{helper}\n    {branch}"), 1);
    for (shape, offset) in [(0x1000, 0), (0x2000, 32)] {
        assert!(guarded_property_path(&with_hot_helper, shape, Some(offset))
            .unwrap_err()
            .contains("helper"));
    }
}

#[test]
fn megamorphic_property_site_uses_the_generic_helper_instead_of_guards() {
    let fixture = SnapshotFixture::compile("(function(o){return o.answer})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let pc = verified
        .instructions()
        .iter()
        .find(|i| i.opcode().name() == "get_field")
        .unwrap()
        .pc();
    let key = FunctionKey::new(1, 1);
    let mut table = ShapeFeedbackTable::new(3);
    for identity in 1..=4 {
        table.observe(
            key,
            pc,
            ShapeObservation::new(
                ShapeToken::new(identity, 1),
                PrototypeDependencyToken::new(0, 0),
                identity as u32,
                PropertyAttributes::WRITABLE,
                ObservedType::Int32,
            ),
        );
    }
    // A megamorphic site has no shape contract to guard. It lowers to the
    // owning GET_PROPERTY bridge rather than failing the whole function or
    // deoptimizing on every unobserved layout.
    let clif = Tier2Compiler::host(1)
        .lower_with_feedback_for_test(
            &verified,
            key,
            &FeedbackSnapshot::empty(1).with_properties(table.snapshot(key)),
        )
        .expect("megamorphic site lowers through the generic helper");
    assert!(clif.contains("call_indirect"), "{clif}");
}

#[test]
fn inherited_and_accessor_values_fail_closed_but_owned_objects_use_the_owning_bridge() {
    let fixture = SnapshotFixture::compile("(function(o){return o.answer})");
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let pc = verified
        .instructions()
        .iter()
        .find(|i| i.opcode().name() == "get_field")
        .unwrap()
        .pc();
    for (prototype, attrs) in [
        (
            PrototypeDependencyToken::new(2, 1),
            PropertyAttributes::WRITABLE,
        ),
        (
            PrototypeDependencyToken::new(0, 0),
            PropertyAttributes::ACCESSOR,
        ),
    ] {
        let key = FunctionKey::new(1, 1);
        let mut table = ShapeFeedbackTable::new(3);
        table.observe(
            key,
            pc,
            ShapeObservation::new(
                ShapeToken::new(0x1000, 7),
                prototype,
                1,
                attrs,
                ObservedType::Int32,
            ),
        );
        let feedback = FeedbackSnapshot::empty(1).with_properties(table.snapshot(key));
        assert!(Tier2Compiler::host(1)
            .lower_with_feedback_for_test(&verified, key, &feedback)
            .is_err());
    }

    let key = FunctionKey::new(1, 1);
    let mut table = ShapeFeedbackTable::new(3);
    table.observe(
        key,
        pc,
        ShapeObservation::new(
            ShapeToken::new(0x1000, 7),
            PrototypeDependencyToken::new(0, 0),
            1,
            PropertyAttributes::WRITABLE,
            ObservedType::Object,
        ),
    );
    let clif = Tier2Compiler::host(1)
        .lower_with_feedback_for_test(
            &verified,
            key,
            &FeedbackSnapshot::empty(1).with_properties(table.snapshot(key)),
        )
        .expect("a refcounted result must use the audited owning helper bridge");
    assert!(
        clif.matches("call_indirect").count() >= 2,
        "the owning bridge must call GET_PROPERTY and then release its consumed receiver: {clif}"
    );

    use rquickjs::{Context, Function, Object, Runtime};
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
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.child={marker:73};globalThis.holder={answer:child};function owned(o){return o.answer}",
            )
        })
        .unwrap();
    for _ in 0..10_000 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("owned").unwrap();
            let holder: Object = ctx.globals().get("holder").unwrap();
            let result: Object = f.call((holder,)).unwrap();
            assert_eq!(result.get::<_, i32>("marker").unwrap(), 73);
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
}

#[test]
fn borrowed_property_store_preserves_live_aliases_and_deopt_owners() {
    use rquickjs::{Context, Function, Object, Runtime};
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
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.left={answer:0};globalThis.right={answer:42};
         function copy(o,other){o.answer=other.answer;return o.answer}",
            )
        })
        .unwrap();
    let call = || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("copy").unwrap();
            let left: Object = ctx.globals().get("left").unwrap();
            let right: Object = ctx.globals().get("right").unwrap();
            assert_eq!(f.call::<_, i32>((left, right)).unwrap(), 42);
        })
    };
    for _ in 0..10_000 {
        call();
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let before = jit.metrics();
    assert!(before.tier2_entries > 0, "{before:?}");
    // A borrowed destination remains live below the source property's operand.
    // Fresh objects must be reclaimed after both guarded loads and stores.
    runtime.run_gc();
    let objects_before = runtime.memory_usage().obj_count;
    for _ in 0..1_024 {
        context
            .with(|ctx| {
                ctx.eval::<(), _>("if(copy({answer:0},{answer:42})!==42)throw Error('copy')")
            })
            .unwrap();
    }
    runtime.run_gc();
    assert_eq!(runtime.memory_usage().obj_count, objects_before);
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    // Keep the same shape but change the property's value to a heap reference.
    // Both operands must acquire owners before the interpreter resumes the copy.
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "right.answer='forty'+String(2);
             if(copy(left,right)!=='forty2')throw Error('type miss')",
            )
        })
        .unwrap();
    assert!(jit.metrics().deopts > before.deopts);
    context
        .with(|ctx| {
            ctx.eval::<(), _>(
                "Object.defineProperty(right,'answer',{get(){throw Error('getter')}});
         try {copy(left,right);throw Error('missing exception')} catch(e) {
             if(e.message!=='getter')throw e;
         }",
            )
        })
        .unwrap();
    runtime.run_gc();
}

#[test]
fn temporary_receivers_keep_shared_shape_guards_stable_across_gc() {
    use rquickjs::{Array, Context, Function, Object, Runtime};
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
    context
        .with(|ctx| {
            ctx.eval::<(), _>("globalThis.shared={answer:41};function read(o){return o.answer}")
        })
        .unwrap();
    for _ in 0..10_000 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("read").unwrap();
            let o: Object = ctx.globals().get("shared").unwrap();
            assert_eq!(f.call::<_, i32>((o,)).unwrap(), 41);
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let before = jit.metrics();
    assert!(before.tier2_entries > 0, "{before:?}");
    context
        .with(|ctx| ctx.eval::<(), _>("shared=null;globalThis.blockers=null"))
        .unwrap();
    runtime.run_gc();
    let objects_before = runtime.memory_usage().obj_count;
    // Occupy freed shape allocations with distinct live layouts, so pointer
    // reuse cannot accidentally make an unrooted old guard match again.
    context.with(|ctx| {
        let blockers = Array::new(ctx.clone()).unwrap();
        for index in 0..128 {
            let object = Object::new(ctx.clone()).unwrap();
            object.set(format!("padding{index}"), index).unwrap();
            blockers.set(index, object).unwrap();
        }
        ctx.globals().set("blockers", blockers).unwrap();
    });
    for index in 0..1_024 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("read").unwrap();
            let object = Object::new(ctx.clone()).unwrap();
            object.set("answer", index).unwrap();
            assert_eq!(f.call::<_, i32>((object,)).unwrap(), index);
        });
        if index % 32 == 0 {
            runtime.run_gc();
        }
        jit.poll();
    }
    let after = jit.metrics();
    assert_eq!(after.deopts, before.deopts, "{before:?} -> {after:?}");
    assert!(
        after.tier2_entries >= before.tier2_entries + 1_024,
        "{after:?}"
    );
    context
        .with(|ctx| ctx.eval::<(), _>("blockers=null"))
        .unwrap();
    runtime.run_gc();
    assert_eq!(runtime.memory_usage().obj_count, objects_before);
}

#[test]
fn borrowed_comparator_second_read_deopt_preserves_live_primitive() {
    use rquickjs::{Context, Function, Object, Runtime};
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
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.left={score:17};globalThis.right={score:42};
             function compare(left,right){return right.score-left.score}",
        )
        .unwrap();
    });
    for _ in 0..10_000 {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("compare").unwrap();
            let left: Object = ctx.globals().get("left").unwrap();
            let right: Object = ctx.globals().get("right").unwrap();
            assert_eq!(f.call::<_, i32>((left, right)).unwrap(), 25);
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let before = jit.metrics();
    assert!(before.tier2_entries > 0, "{before:?}");
    runtime.run_gc();
    let objects_before = runtime.memory_usage().obj_count;
    for _ in 0..128 {
        context.with(|ctx| {
            ctx.eval::<(), _>("if(compare({score:17},{score:42})!==25)throw Error('compare')")
                .unwrap();
        });
    }
    runtime.run_gc();
    assert_eq!(runtime.memory_usage().obj_count, objects_before);
    let hits = jit.metrics();
    assert!(hits.tier2_entries > before.tier2_entries, "{hits:?}");
    assert_eq!(hits.deopts, before.deopts, "{before:?} -> {hits:?}");
    // Preserve the shape but fail the second property's primitive tag guard.
    // Exact resumption needs the first result (42) and the left receiver;
    // valueOf exercises interpreter coercion after the exact resumption.
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "left.score={valueOf(){return 19}};
             if(compare(left,right)!==23)throw Error('second read deopt')",
        )
        .unwrap();
    });
    assert!(jit.metrics().deopts > hits.deopts, "{:?}", jit.metrics());
    runtime.run_gc();
}
