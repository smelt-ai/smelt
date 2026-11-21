#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Function, Object, Runtime};
use rquickjs_jit::{Jit, JitConfig};

fn setup(source: &str) -> (Runtime, Jit, Context) {
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
    context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
    (runtime, jit, context)
}

fn warm(jit: &Jit, mut call: impl FnMut()) {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 75 } else { 15 });
    loop {
        call();
        jit.poll();
        if jit.metrics().tier2_entries >= 4 {
            return;
        }
        assert_eq!(
            jit.metrics().blacklisted,
            0,
            "kernel compilation was blacklisted: {:?}",
            jit.metrics()
        );
        assert!(
            std::time::Instant::now() < deadline,
            "Tier2 kernel not ready: {:?}",
            jit.metrics()
        );
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
}

const STRUCTURAL_SHAPE: u64 = 0x1234_5000;

fn property_clif(source: &str, object_argument: usize, include_stores: bool) -> (String, usize) {
    use rquickjs_jit::{
        bytecode::VerifyLimits,
        compiler::optimized::Tier2Compiler,
        runtime::{
            FeedbackTable, FunctionKey, ObservedType, PropertyAttributes, PrototypeDependencyToken,
            ShapeFeedbackTable, ShapeObservation, ShapeToken,
        },
        test_support::SnapshotFixture,
    };
    let fixture = SnapshotFixture::compile(source);
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let mut feedback = FeedbackTable::new(64, 2);
    let mut shapes = ShapeFeedbackTable::new(3);
    for instruction in verified.instructions() {
        if instruction.opcode().name() == "get_field"
            || (include_stores && instruction.opcode().name() == "put_field")
        {
            shapes.observe(
                key,
                instruction.pc(),
                ShapeObservation::new(
                    ShapeToken::new(STRUCTURAL_SHAPE, 7),
                    PrototypeDependencyToken::new(0, 0),
                    0,
                    PropertyAttributes::WRITABLE,
                    ObservedType::Int32,
                ),
            );
        }
        if instruction.opcode().name() == "add" {
            for _ in 0..32 {
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
    }
    let mut arguments = vec![ObservedType::Int32; usize::from(verified.snapshot().arg_count())];
    arguments[object_argument] = ObservedType::Object;
    for _ in 0..32 {
        feedback.observe_call(key, &arguments);
    }
    let clif = Tier2Compiler::host(701)
        .lower_with_feedback_for_test(
            &verified,
            key,
            &feedback.snapshot(701).with_properties(shapes.snapshot(key)),
        )
        .unwrap();
    let local_writes = verified
        .instructions()
        .iter()
        .filter(|instruction| {
            instruction.opcode().name().starts_with("put_loc")
                || instruction.opcode().name().starts_with("set_loc")
        })
        .count();
    (clif, local_writes)
}

fn property_read_clif(source: &str) -> (String, usize) {
    property_clif(source, 0, false)
}

fn minimum_native_shape_checks(clif: &str) -> Option<usize> {
    use std::collections::{BTreeMap, BTreeSet};

    // Analyze emitted control flow, excluding runtime/deopt paths. Lazy caches
    // may retain several static miss blocks; successful reuse must be able to
    // bypass later live shape loads. Native tests separately establish that
    // the valid-state branch is selected only after a successful first guard.
    let mut blocks = BTreeMap::<u32, Vec<&str>>::new();
    let mut constants = BTreeMap::<&str, i32>::new();
    let mut aliases = BTreeMap::<&str, &str>::new();
    let mut current = None;
    for line in clif.lines().map(str::trim) {
        if let Some((value, instruction)) = line.split_once(" = iconst.") {
            if let Some((_, immediate)) = instruction.split_once(' ') {
                if let Ok(immediate) = immediate.parse() {
                    constants.insert(value, immediate);
                }
            }
        }
        if let Some((value, source)) = line.split_once(" -> ") {
            aliases.insert(value, source);
        }
        if let Some(label) = line.strip_prefix("block") {
            let id = label
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .unwrap();
            current = Some(id);
            blocks.entry(id).or_default();
        } else if let Some(id) = current {
            blocks.get_mut(&id).unwrap().push(line);
        }
    }
    let shape_test = |line: &&str| {
        if !line.contains("icmp_imm eq") {
            return false;
        }
        let Some((_, immediate)) = line.rsplit_once(", ") else {
            return false;
        };
        let immediate = immediate.replace('_', "");
        let value = if let Some(hex) = immediate.strip_prefix("0x") {
            u64::from_str_radix(hex, 16).ok()
        } else {
            immediate.parse().ok()
        };
        value == Some(STRUCTURAL_SHAPE)
    };
    assert!(
        blocks
            .values()
            .flat_map(|lines| lines.iter())
            .any(shape_test),
        "missing guarded field path: {clif}"
    );
    let mut pending = BTreeSet::from([(0usize, 0u32)]);
    let mut visited = BTreeSet::new();
    let mut least_checks = None;
    while let Some((checks, id)) = pending.pop_first() {
        if !visited.insert(id) {
            continue;
        }
        let lines = &blocks[&id];
        if lines.iter().any(|line| line.contains("call_indirect")) {
            continue;
        }
        let checks = checks + lines.iter().filter(|line| shape_test(line)).count();
        if lines.iter().any(|line| line.starts_with("return")) {
            // A helper-free deopt is still an exit, not a successful result.
            // The first ABI argument is sret and success writes EXIT_DONE.
            if lines.iter().any(|line| {
                let operation = line.split("  ;").next().unwrap();
                operation
                    .strip_prefix("store ")
                    .or_else(|| operation.strip_prefix("store.i32 "))
                    .and_then(|store| store.strip_suffix(", v0"))
                    .and_then(|mut value| {
                        // Constants may be canonicalized into aliases.
                        for _ in 0..aliases.len() {
                            let Some(source) = aliases.get(value) else {
                                break;
                            };
                            value = source;
                        }
                        constants.get(value)
                    })
                    .copied()
                    == Some(rquickjs::qjs::JSJitExitKind_JS_JIT_EXIT_DONE as i32)
            }) {
                least_checks = Some(checks);
                break;
            }
            continue;
        }
        for line in lines.iter().filter(|line| {
            line.starts_with("brif ") || line.starts_with("brif.") || line.starts_with("jump ")
        }) {
            let mut known_branch = None;
            if line.starts_with("brif") {
                if let Some(condition) = line.split_whitespace().nth(1) {
                    let mut condition = condition.trim_end_matches(',');
                    for _ in 0..aliases.len() {
                        let Some(source) = aliases.get(condition) else {
                            break;
                        };
                        condition = source;
                    }
                    known_branch = constants
                        .get(condition)
                        .map(|value| usize::from(*value == 0));
                }
            }
            for (edge, label) in line.split("block").skip(1).enumerate() {
                // Invalidation writes valid=false. Such a branch cannot use
                // the cache-hit edge merely because it exists in the CFG.
                if known_branch.is_some_and(|selected| selected != edge) {
                    continue;
                }
                let next = label
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u32>()
                    .unwrap();
                pending.insert((checks, next));
            }
        }
    }
    least_checks
}

fn continuing_property_loop_is_field_free(clif: &str) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    #[derive(Default)]
    struct Block<'a> {
        cold: bool,
        lines: Vec<&'a str>,
        successors: Vec<u32>,
    }
    fn block_id(line: &str) -> Option<u32> {
        line.trim()
            .strip_prefix("block")?
            .split(['(', ':', ' '])
            .next()?
            .parse()
            .ok()
    }
    fn referenced_blocks(line: &str) -> Vec<u32> {
        let mut result = Vec::new();
        let mut rest = line;
        while let Some(index) = rest.find("block") {
            rest = &rest[index + 5..];
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if let Ok(block) = digits.parse() {
                result.push(block);
            }
            rest = &rest[digits.len()..];
        }
        result
    }

    let mut blocks = BTreeMap::<u32, Block<'_>>::new();
    let mut current = None;
    for line in clif.lines() {
        if let Some(id) = block_id(line) {
            current = Some(id);
            blocks.entry(id).or_default().cold = line.contains(" cold:");
        }
        let Some(id) = current else { continue };
        let block = blocks.entry(id).or_default();
        block.lines.push(line);
        if line.trim_start().starts_with("jump ") || line.trim_start().starts_with("brif") {
            block.successors.extend(referenced_blocks(line));
        }
    }
    let header = blocks
        .iter()
        .find_map(|(&id, block)| {
            block
                .lines
                .iter()
                .any(|line| {
                    line.contains("iadd_imm")
                        && line
                            .split("  ;")
                            .next()
                            .unwrap_or(line)
                            .trim()
                            .ends_with(", -1")
                })
                .then_some(id)
        })
        .ok_or("missing countdown loop header")?;
    let latch = blocks
        .iter()
        .rev()
        .find_map(|(&id, block)| block.successors.contains(&header).then_some(id))
        .filter(|&id| id != header)
        .ok_or("missing continuing backedge")?;

    let mut queue = VecDeque::from([(header, vec![header])]);
    let mut seen = BTreeSet::from([header]);
    let path = loop {
        let Some((block, path)) = queue.pop_front() else {
            return Err("no non-cold header-to-latch path".into());
        };
        if block == latch {
            break path;
        }
        for &successor in &blocks[&block].successors {
            if blocks.get(&successor).is_some_and(|next| !next.cold) && seen.insert(successor) {
                let mut next = path.clone();
                next.push(successor);
                queue.push_back((successor, next));
            }
        }
    };
    if path.len() < 3 {
        return Err(format!("continuing path is unexpectedly trivial: {path:?}"));
    }
    for id in path {
        for line in &blocks[&id].lines {
            let instruction = line.trim();
            if instruction.contains(&format!("{STRUCTURAL_SHAPE:#x}"))
                || (instruction.contains("load.i64") && !instruction.contains("stack_load.i64"))
                || (instruction.starts_with("store.i64")
                    && instruction
                        .split_once(',')
                        .is_some_and(|(_, address)| !address.contains('+')))
                || instruction.starts_with("brif.i8")
            {
                return Err(format!(
                    "block{id} repeats a field/cache operation: {instruction}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn repeated_property_reads_have_a_native_path_reusing_the_guarded_field() {
    let (clif, _) = property_read_clif("(function(o){return o.x+o.x+o.x})");
    let least_checks = minimum_native_shape_checks(&clif);
    assert!(least_checks.is_some_and(|checks| checks <= 1),
        "three identical reads must reuse a guarded field on a native path; least checks={least_checks:?}: {clif}");
}

#[test]
fn primitive_local_assignments_preserve_a_previously_guarded_property() {
    let (clif, local_writes) =
        property_read_clif("(function(o,seed){let v=o.x;let t=seed+1;return o.x+v+t})");
    assert!(
        local_writes >= 2,
        "fixture must execute real local assignments"
    );
    let least_checks = minimum_native_shape_checks(&clif);
    assert_eq!(least_checks, Some(1),
        "primitive local writes must preserve the first guarded property; native checks={least_checks:?}: {clif}");
}

#[test]
fn guarded_property_loop_uses_a_cold_amortized_poll() {
    let (clif, _) = property_clif(
        "(function(n,seed,o){o.x=seed;o.y=1;let toggle=0;for(let i=0;i<n;i++){o.x=o.x+o.y;o.y=o.y+toggle;toggle=1-toggle;}return o.x+o.y})",
        2,
        true,
    );
    let lines = clif.lines().collect::<Vec<_>>();
    let decrement = lines
        .iter()
        .position(|line| {
            let line = line.split("  ;").next().unwrap_or(line).trim();
            line.contains("iadd_imm") && line.ends_with(", -1")
        })
        .expect("guarded scalar/property leaves must have a poll countdown");
    // The countdown is an SSA value carried by a loop-header block
    // parameter, so it stays in a register instead of a stack-slot
    // load/decrement/store on every iteration.
    let operand = lines[decrement]
        .split("  ;")
        .next()
        .and_then(|line| line.split("iadd_imm").nth(1))
        .and_then(|rest| rest.split(',').next())
        .map(str::trim)
        .expect("countdown decrement operand");
    let header = lines[..decrement]
        .iter()
        .rev()
        .find(|line| line.trim_start().starts_with("block"))
        .expect("countdown decrement inside a block");
    assert!(
        header.contains(&format!("{operand}: i32")),
        "countdown must be a loop-header block parameter, not memory: {header}; {}",
        lines[decrement]
    );
    assert!(
        !clif.contains("stack_load") && !clif.contains("stack_store"),
        "countdown must not round-trip through a stack slot: {clif}"
    );

    assert_eq!(
        continuing_property_loop_is_field_free(&clif),
        Ok(()),
        "{clif}"
    );
}

#[test]
fn continuing_property_loop_check_rejects_an_injected_field_access() {
    let (clif, _) = property_clif(
        "(function(n,seed,o){o.x=seed;o.y=1;let toggle=0;for(let i=0;i<n;i++){o.x=o.x+o.y;o.y=o.y+toggle;toggle=1-toggle;}return o.x+o.y})",
        2,
        true,
    );
    assert_eq!(continuing_property_loop_is_field_free(&clif), Ok(()));
    let countdown = clif
        .lines()
        .find(|line| {
            let line = line.split("  ;").next().unwrap_or(line).trim();
            line.contains("iadd_imm") && line.ends_with(", -1")
        })
        .expect("fixture must contain the loop countdown");
    let mutated = clif.replacen(
        countdown,
        &format!("{countdown}\n    v9999 = load.i64 v0+24"),
        1,
    );
    assert!(
        continuing_property_loop_is_field_free(&mutated)
            .unwrap_err()
            .contains("repeats a field/cache operation"),
        "the structural check must reject a field load inserted in the hot loop"
    );
}

#[test]
fn replacing_an_owned_local_keeps_property_effects_and_releases_the_old_value() {
    let (runtime, jit, context) = setup(
        "globalThis.shared={x:1,y:2};function factory(){return {payload:1}}
         function target(o,make,seed){let owned=make();o.x=o.x+1;
             let v=o.x;owned=seed+1;return o.x+v+owned}",
    );
    let call = || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let factory: Function = ctx.globals().get("factory").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 1).unwrap();
            assert_eq!(f.call::<_, i32>((object.clone(), factory, 3)).unwrap(), 8);
            assert_eq!(object.get::<_, i32>("x").unwrap(), 2);
        })
    };
    // An object-producing call may retain its general bridge. Replacing its
    // owning result must still release that result and commit prior writes.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 75 } else { 15 });
    while jit.metrics().native_entries < 32 {
        call();
        jit.poll();
        assert!(
            std::time::Instant::now() < deadline,
            "owning-local kernel did not enter native code: {:?}",
            jit.metrics()
        );
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    runtime.run_gc();
    let objects_before = runtime.memory_usage().obj_count;
    for _ in 0..256 {
        call();
    }
    runtime.run_gc();
    assert_eq!(
        runtime.memory_usage().obj_count,
        objects_before,
        "overwriting the owning local leaked the old object"
    );
    assert!(jit.metrics().native_entries > 0);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn stable_property_loop_materializes_fields_on_normal_early_and_zero_trip_exits() {
    let (_runtime, jit, context) = setup(
        "globalThis.shared={x:1,y:2};
         function target(o,n,stop){for(let i=0;i<n;i++){
             o.x=o.x+1;o.y=o.y+o.x;if(i===stop)return o.x;
         }return o.x+o.y}",
    );
    let call = |n, stop, expected, x, y| {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 1).unwrap();
            object.set("y", 2).unwrap();
            assert_eq!(
                f.call::<_, i32>((object.clone(), n, stop)).unwrap(),
                expected
            );
            assert_eq!(object.get::<_, i32>("x").unwrap(), x);
            assert_eq!(object.get::<_, i32>("y").unwrap(), y);
        })
    };
    // The early return has its own get_field site. Populate both paths before
    // the maintenance poll snapshots property feedback for compilation.
    warm(&jit, || {
        call(3, -1, 15, 4, 11);
        call(5, 1, 3, 3, 7);
    });
    let before = jit.metrics();
    call(5, -1, 28, 6, 22);
    call(5, 1, 3, 3, 7);
    call(0, -1, 3, 1, 2);
    call(-1, -1, 3, 1, 2);
    let after = jit.metrics();
    assert!(
        after.tier2_entries >= before.tier2_entries + 4,
        "{before:?} -> {after:?}"
    );
    assert_eq!(
        after.deopts, before.deopts,
        "stable field loop exited native code"
    );
    assert_eq!(after.native_entries, after.native_exits);
}

#[test]
fn arithmetic_overflow_after_a_store_preserves_the_effect_exactly_once() {
    let (_runtime, jit, context) = setup(
        "globalThis.shared={x:0,y:0};
         function target(o,n){o.x=o.x+1;let v=n+1;o.y=v;return o.x+o.y}",
    );
    warm(&jit, || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 0).unwrap();
            object.set("y", 0).unwrap();
            assert_eq!(f.call::<_, i32>((object, 2)).unwrap(), 4);
        })
    });
    let before = jit.metrics();
    context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let object: Object = ctx.globals().get("shared").unwrap();
        object.set("x", 10).unwrap();
        object.set("y", 0).unwrap();
        assert_eq!(
            f.call::<_, f64>((object.clone(), i32::MAX)).unwrap(),
            2_147_483_659.0
        );
        assert_eq!(object.get::<_, i32>("x").unwrap(), 11);
        assert_eq!(object.get::<_, f64>("y").unwrap(), 2_147_483_648.0);
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert!(
        jit.metrics().deopts > before.deopts,
        "overflow must recover after the first write"
    );
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn property_update_overflow_restores_receiver_below_arithmetic_operands() {
    let (_runtime, jit, context) =
        setup("globalThis.shared={x:0,y:0};function target(o){o.y=o.y+1;o.x=o.x+1;return o.x}");
    warm(&jit, || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 1).unwrap();
            object.set("y", 0).unwrap();
            assert_eq!(f.call::<_, i32>((object,)).unwrap(), 2);
        })
    });
    let before = jit.metrics();
    context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let object: Object = ctx.globals().get("shared").unwrap();
        object.set("x", i32::MAX).unwrap();
        object.set("y", 10).unwrap();
        let result = f.call::<_, f64>((object.clone(),));
        assert!(
            result.is_ok(),
            "overflow lost the receiver below lhs/rhs: {result:?}; {:?}",
            ctx.catch()
        );
        assert_eq!(result.unwrap(), 2_147_483_648.0);
        assert_eq!(object.get::<_, f64>("x").unwrap(), 2_147_483_648.0);
        assert_eq!(
            object.get::<_, i32>("y").unwrap(),
            11,
            "earlier field write replayed or lost"
        );
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert!(jit.metrics().deopts > before.deopts);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn different_argument_numbers_do_not_disambiguate_the_same_object() {
    let (_runtime, jit, context) = setup(
        "globalThis.left={x:1,y:2};globalThis.right={x:5,y:7};
         function target(a,b){a.x=a.x+1;b.x=b.x+10;return a.x+b.x+a.y}",
    );
    warm(&jit, || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let a: Object = ctx.globals().get("left").unwrap();
            let b: Object = ctx.globals().get("right").unwrap();
            a.set("x", 1).unwrap();
            b.set("x", 5).unwrap();
            assert_eq!(f.call::<_, i32>((a, b)).unwrap(), 19);
        })
    });
    let before = jit.metrics();
    context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let a: Object = ctx.globals().get("left").unwrap();
        a.set("x", 1).unwrap();
        assert_eq!(f.call::<_, i32>((a.clone(), a.clone())).unwrap(), 26);
        assert_eq!(a.get::<_, i32>("x").unwrap(), 12);
        assert_eq!(a.get::<_, i32>("y").unwrap(), 2);
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn loop_backedge_alias_store_invalidates_a_preheader_seed() {
    let (_runtime, jit, context) = setup(
        "globalThis.left={x:0};globalThis.right={x:0};
         function target(a,b,n){a.x=1;let s=0;for(let i=0;i<n;i++){s+=a.x;b.x=2;}return s}",
    );
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 75 } else { 15 });
    loop {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let a: Object = ctx.globals().get("left").unwrap();
            let b: Object = ctx.globals().get("right").unwrap();
            assert_eq!(f.call::<_, i32>((a, b, 3)).unwrap(), 3);
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        assert_eq!(
            jit.metrics().blacklisted,
            0,
            "kernel compilation was blacklisted: {:?}",
            jit.metrics()
        );
        assert!(
            std::time::Instant::now() < deadline,
            "Tier2 kernel not ready: {:?}",
            jit.metrics()
        );
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    let before = jit.metrics();
    context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let object: Object = ctx.globals().get("left").unwrap();
        assert_eq!(f.call::<_, i32>((object.clone(), object, 3)).unwrap(), 5);
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn unknown_call_observes_pending_store_and_invalidates_cached_values() {
    let (_runtime, jit, context) = setup(
        "globalThis.shared={x:1,y:2};globalThis.seen=0;
         function mutate(o){seen=o.x;o.x=o.x+10}
         function target(o,f){o.x=o.x+1;f(o);return o.x+o.y}",
    );
    let call = || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let mutate: Function = ctx.globals().get("mutate").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 1).unwrap();
            assert_eq!(f.call::<_, i32>((object.clone(), mutate)).unwrap(), 14);
            assert_eq!(ctx.globals().get::<_, i32>("seen").unwrap(), 2);
            assert_eq!(object.get::<_, i32>("x").unwrap(), 12);
        })
    };
    // The effectful callee may use the generic bridge. Correctness does not
    // require general inlining, and the rooted property-only kernel is tested
    // independently for native readiness above.
    for _ in 0..256 {
        call();
        jit.poll();
    }
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "shared.x=1;
             try {target(shared,function(o){if(o.x!==2)throw Error('stale');
                 o.x=20;throw Error('after mutation')});throw Error('missing throw')}
             catch(e){if(e.message!=='after mutation')throw e}
             if(shared.x!==20)throw Error('lost mutation')",
        )
        .unwrap();
    });
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn accessor_proxy_and_nonwritable_transitions_preserve_effect_order() {
    for probe in [
        "let events=[];let value=1;
         Object.defineProperty(shared,'x',{get(){events.push('get');return value},
             set(v){events.push('set:'+v);value=v}});
         if(target(shared)!==4||value!==2||events.join(',')!=='get,set:2,get')throw Error(events)",
        "let events=[];let proxy=new Proxy(shared,{
             get(o,k){events.push('get:'+k);return o[k]},
             set(o,k,v){events.push('set:'+k);o[k]=v;return true}});
         if(target(proxy)!==4||events.join(',')!=='get:x,set:x,get:x,get:y')throw Error(events)",
        "Object.defineProperty(shared,'x',{writable:false});
         try{target(shared);throw Error('missing throw')}catch(e){if(!(e instanceof TypeError))throw e}
         if(shared.x!==1||shared.y!==2)throw Error('nonwritable mutation')",
        "shared.extra=9;if(target(shared)!==4||shared.x!==2||shared.y!==2)throw Error('shape transition')",
    ] {
        let (runtime, jit, context) = setup(
            "globalThis.shared={x:1,y:2};function target(o){'use strict';o.x=o.x+1;return o.x+o.y}",
        );
        warm(&jit, || context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 1).unwrap();
            assert_eq!(f.call::<_, i32>((object,)).unwrap(), 4);
        }));
        context.with(|ctx| {
            ctx.eval::<(), _>("shared.x=1").unwrap();
            ctx.eval::<(), _>(probe).unwrap_or_else(|error| panic!("{probe}: {error}; {:?}", ctx.catch()));
        });
        runtime.run_gc();
        assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
    }
}

#[test]
fn generic_getter_cannot_preserve_a_stale_argument_receiver() {
    let (_runtime, jit, context) = setup(
        "globalThis.first={x:1};globalThis.plain={x:0};
         function target(o,bad){bad.x;return o.x}",
    );
    warm(&jit, || {
        context.with(|ctx| {
            let target: Function = ctx.globals().get("target").unwrap();
            let first: Object = ctx.globals().get("first").unwrap();
            let plain: Object = ctx.globals().get("plain").unwrap();
            assert_eq!(target.call::<_, i32>((first, plain)).unwrap(), 1);
        })
    });
    let before = jit.metrics();
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "let bad={get x(){first.x=9;return 0}};
             if(target(first,bad)!==9)throw Error('stale receiver value')",
        )
        .unwrap();
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn interrupt_observes_committed_fields_and_mutation_invalidates_loop_cache() {
    use rquickjs::qjs;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    let (runtime, jit, context) = setup(
        "globalThis.shared={x:0,y:0};function target(o,n){
             for(let i=0;i<n;i++){o.x=o.x+1;o.y=o.x}return o.x+o.y}",
    );
    warm(&jit, || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 0).unwrap();
            object.set("y", 0).unwrap();
            assert_eq!(f.call::<_, i32>((object, 3)).unwrap(), 6);
        })
    });
    let (raw_context, raw_object) = context.with(|ctx| {
        let object: Object = ctx.globals().get("shared").unwrap();
        object.set("x", 0).unwrap();
        object.set("y", 0).unwrap();
        (ctx.as_raw().as_ptr() as usize, unsafe {
            qjs::JS_VALUE_GET_PTR(object.as_value().as_raw())
        } as usize)
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let mutated = Arc::new(AtomicBool::new(false));
    let invalid = Arc::new(AtomicBool::new(false));
    let (observed_calls, did_mutate, invalid_value) =
        (calls.clone(), mutated.clone(), invalid.clone());
    // The context and global object stay rooted until after the handler is
    // removed. These C calls operate only on existing own writable Int32 data
    // properties of a plain object: no accessor, Proxy, script evaluation or
    // Rust runtime lock reentry is possible. Avoid Context::with in a callback
    // because the outer invocation already owns the runtime lock.
    runtime.set_interrupt_handler(Some(Box::new(move || unsafe {
        observed_calls.fetch_add(1, Ordering::SeqCst);
        let ctx = raw_context as *mut qjs::JSContext;
        let object = qjs::JS_MKPTR(qjs::JS_TAG_OBJECT, raw_object as *mut core::ffi::c_void);
        let x = qjs::JS_GetPropertyStr(ctx, object, c"x".as_ptr());
        let y = qjs::JS_GetPropertyStr(ctx, object, c"y".as_ptr());
        let primitive = qjs::JS_VALUE_GET_TAG(x) == qjs::JS_TAG_INT
            && qjs::JS_VALUE_GET_TAG(y) == qjs::JS_TAG_INT;
        if !primitive {
            qjs::JS_FreeValue(ctx, x);
            qjs::JS_FreeValue(ctx, y);
            invalid_value.store(true, Ordering::SeqCst);
            return true;
        }
        let current = qjs::JS_VALUE_GET_INT(x);
        let completed = qjs::JS_VALUE_GET_INT(y);
        qjs::JS_FreeValue(ctx, x);
        qjs::JS_FreeValue(ctx, y);
        if current > 0
            && current == completed
            && did_mutate
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            && qjs::JS_SetPropertyStr(
                ctx,
                object,
                c"x".as_ptr(),
                qjs::JS_MKVAL(qjs::JS_TAG_INT, current + 10_000),
            ) < 0
        {
            invalid_value.store(true, Ordering::SeqCst);
            return true;
        }
        false
    })));
    let before = jit.metrics();
    let result = context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let object: Object = ctx.globals().get("shared").unwrap();
        // The native poll is amortized at 64 loop iterations and QuickJS
        // invokes the host interrupt callback after its own 10,000 poll ticks.
        f.call::<_, i32>((object, 1_000_000))
    });
    runtime.set_interrupt_handler(None);
    assert!(!invalid.load(Ordering::SeqCst));
    assert!(calls.load(Ordering::SeqCst) > 0);
    assert!(
        mutated.load(Ordering::SeqCst),
        "interrupt never observed a completed field update"
    );
    assert_eq!(
        result.unwrap(),
        2_020_000,
        "the next iteration must reload the callback's numeric write"
    );
    context.with(|ctx| {
        let object: Object = ctx.globals().get("shared").unwrap();
        assert_eq!(object.get::<_, i32>("x").unwrap(), 1_010_000);
        assert_eq!(object.get::<_, i32>("y").unwrap(), 1_010_000);
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn guarded_property_loop_keeps_its_induction_variable_int32() {
    // Guarded property leaves cannot reenter JavaScript, so the loop counter
    // read across them keeps its Int32 seed: no Float64 fallback for `i++`
    // and an integer loop comparison.
    let (clif, _) = property_clif(
        "(function(n,seed,o){o.x=seed;for(let i=0;i<n;i++){o.x=o.x+1;}return o.x})",
        2,
        true,
    );
    for float_op in ["fcvt_from_sint", "fcmp", "fadd"] {
        assert!(
            !clif.contains(float_op),
            "induction variable fell back to Float64 ({float_op}): {clif}"
        );
    }
    assert!(clif.contains("icmp slt"), "{clif}");
    assert_eq!(
        continuing_property_loop_is_field_free(&clif),
        Ok(()),
        "{clif}"
    );
}

#[test]
fn specialized_property_loop_counter_overflow_and_float_seed_deoptimize_exactly() {
    let (_runtime, jit, context) = setup(
        "globalThis.shared={x:0};function target(o,start,n){
             for(let i=start;i<=n;i++){o.x=o.x+1}return o.x}",
    );
    warm(&jit, || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 0).unwrap();
            assert_eq!(f.call::<_, i32>((object, 0, 9)).unwrap(), 10);
        })
    });
    let before = jit.metrics();
    context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let object: Object = ctx.globals().get("shared").unwrap();
        // `i++` overflows Int32 after the last iteration: the checked update
        // must deoptimize and let the interpreter finish with a double.
        object.set("x", 0).unwrap();
        assert_eq!(
            f.call::<_, i32>((object.clone(), i32::MAX - 3, i32::MAX))
                .unwrap(),
            4
        );
        assert_eq!(object.get::<_, i32>("x").unwrap(), 4);
        object.set("x", 100).unwrap();
        assert_eq!(f.call::<_, i32>((object.clone(), 0, 4)).unwrap(), 105);
        // A Float64 seed fails the update's operand check instead of being
        // truncated by the Int32 update.
        object.set("x", 0).unwrap();
        assert_eq!(f.call::<_, i32>((object.clone(), 0.5, 3.0)).unwrap(), 3);
        assert_eq!(object.get::<_, i32>("x").unwrap(), 3);
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

#[test]
fn terminating_interrupt_after_unpublished_property_reads_unwinds_cleanly() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    // Property leaves no longer publish their stack operands. A terminating
    // interrupt at the loop poll must still unwind only releasable values.
    let (runtime, jit, context) = setup(
        "globalThis.shared={x:1,y:2};function target(o,n){let s=0;
             for(let i=0;i<n;i++){s=((i&7)+o.x)+(o.y+(i&3));o.x=(o.x+1)&1023}return s}",
    );
    warm(&jit, || {
        context.with(|ctx| {
            let f: Function = ctx.globals().get("target").unwrap();
            let object: Object = ctx.globals().get("shared").unwrap();
            object.set("x", 1).unwrap();
            object.set("y", 2).unwrap();
            assert_eq!(f.call::<_, i32>((object, 3)).unwrap(), 9);
        })
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        observed.fetch_add(1, Ordering::SeqCst) >= 2
    })));
    let before = jit.metrics();
    context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let object: Object = ctx.globals().get("shared").unwrap();
        assert!(f.call::<_, i32>((object, 10_000_000)).is_err());
        let _ = ctx.catch();
    });
    runtime.set_interrupt_handler(None);
    assert!(calls.load(Ordering::SeqCst) > 2);
    context.with(|ctx| {
        let f: Function = ctx.globals().get("target").unwrap();
        let object: Object = ctx.globals().get("shared").unwrap();
        object.set("x", 1).unwrap();
        object.set("y", 2).unwrap();
        assert_eq!(f.call::<_, i32>((object.clone(), 3)).unwrap(), 9);
        assert_eq!(object.get::<_, i32>("x").unwrap(), 4);
    });
    assert!(jit.metrics().tier2_entries > before.tier2_entries);
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
    runtime.run_gc();
}
