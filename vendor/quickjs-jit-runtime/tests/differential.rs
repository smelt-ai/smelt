#![cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use rquickjs::{Context, Function, Runtime};
use rquickjs_jit::bytecode::{
    linked_opcode_table, tier1_policy, FallbackReason, HelperId, Tier1Policy,
};
use rquickjs_jit::test_support::{assert_tier1_rejected, differential};
use rquickjs_jit::{
    correctness::{canonical_observation_source, canonical_observer_prelude},
    JitRuntime,
};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
struct OpcodeManifest {
    cases: Vec<OpcodeCase>,
}

#[derive(Deserialize)]
struct OpcodeCase {
    opcode: String,
    definition: String,
    expression: String,
    helper: Option<ManifestHelper>,
    dimensions: BTreeSet<Dimension>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
enum Dimension {
    Normal,
    NumericTagEdge,
    ExceptionOrNonthrow,
    OwnershipGc,
    CoercionReentrancy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
enum ManifestHelper {
    Dup,
    Free,
    ResolveConst,
    AtomValue,
    ToNumeric,
    ToBool,
    AddSlow,
    BinaryArithSlow,
    UnaryArithSlow,
    CompareSlow,
    GetProperty,
    SetProperty,
    GetElement,
    SetElement,
    ToPropertyKey,
    GetGlobal,
    Call,
    CallConstructor,
    Regexp,
    NewArray,
    NewObject,
    FClosure,
    GetVarRef,
    PutVarRef,
    CloseLocal,
    SetName,
    GenericOp,
    IteratorOp,
}

impl ManifestHelper {
    const fn id(self) -> HelperId {
        match self {
            Self::Dup => HelperId::Dup,
            Self::Free => HelperId::Free,
            Self::ResolveConst => HelperId::ResolveConst,
            Self::AtomValue => HelperId::AtomValue,
            Self::ToNumeric => HelperId::ToNumeric,
            Self::ToBool => HelperId::ToBool,
            Self::AddSlow => HelperId::AddSlow,
            Self::BinaryArithSlow => HelperId::BinaryArithSlow,
            Self::UnaryArithSlow => HelperId::UnaryArithSlow,
            Self::CompareSlow => HelperId::CompareSlow,
            Self::GetProperty => HelperId::GetProperty,
            Self::SetProperty => HelperId::SetProperty,
            Self::GetElement => HelperId::GetElement,
            Self::SetElement => HelperId::SetElement,
            Self::ToPropertyKey => HelperId::ToPropertyKey,
            Self::GetGlobal => HelperId::GetGlobal,
            Self::Call => HelperId::Call,
            Self::CallConstructor => HelperId::CallConstructor,
            Self::Regexp => HelperId::Regexp,
            Self::NewArray => HelperId::NewArray,
            Self::NewObject => HelperId::NewObject,
            Self::FClosure => HelperId::FClosure,
            Self::GetVarRef => HelperId::GetVarRef,
            Self::PutVarRef => HelperId::PutVarRef,
            Self::CloseLocal => HelperId::CloseLocal,
            Self::SetName => HelperId::SetName,
            Self::GenericOp => HelperId::GenericOp,
            Self::IteratorOp => HelperId::IteratorOp,
        }
    }
}

fn required_dimensions(case: &OpcodeCase) -> BTreeSet<Dimension> {
    let mut required = BTreeSet::from([Dimension::Normal, Dimension::ExceptionOrNonthrow]);
    let opcode = linked_opcode_table()
        .find(|opcode| opcode.name() == case.opcode)
        .expect("manifest opcode is linked");
    if matches!(tier1_policy(opcode.id()), Some(Tier1Policy::Helper(_))) {
        required.insert(Dimension::OwnershipGc);
    }
    if matches!(
        case.opcode.as_str(),
        "plus"
            | "post_inc"
            | "post_dec"
            | "add"
            | "sub"
            | "mul"
            | "div"
            | "lt"
            | "lte"
            | "gt"
            | "gte"
            | "eq"
            | "neq"
            | "strict_eq"
            | "strict_neq"
            | "neg"
            | "inc"
            | "dec"
            | "not"
            | "shl"
            | "sar"
            | "shr"
            | "xor"
            | "inc_loc"
            | "dec_loc"
            | "add_loc"
            | "mod"
    ) {
        required.insert(Dimension::NumericTagEdge);
    }
    if matches!(
        case.helper,
        Some(
            ManifestHelper::ToNumeric
                | ManifestHelper::AddSlow
                | ManifestHelper::CompareSlow
                | ManifestHelper::GetProperty
                | ManifestHelper::SetProperty
                | ManifestHelper::GetElement
                | ManifestHelper::SetElement
                | ManifestHelper::ToPropertyKey
                | ManifestHelper::Call
                | ManifestHelper::CallConstructor
                | ManifestHelper::BinaryArithSlow
                | ManifestHelper::UnaryArithSlow
                | ManifestHelper::IteratorOp
        )
    ) {
        required.insert(Dimension::CoercionReentrancy);
    }
    required
}

fn validate_dimensions(manifest: &OpcodeManifest) -> Result<(), String> {
    for case in &manifest.cases {
        let opcode = linked_opcode_table()
            .find(|opcode| opcode.name() == case.opcode)
            .ok_or_else(|| format!("unknown opcode {}", case.opcode))?;
        let expected_helper = match tier1_policy(opcode.id()) {
            Some(Tier1Policy::Helper(helper)) => Some(helper),
            Some(Tier1Policy::Native) => None,
            _ => return Err(format!("{} is not advertised", case.opcode)),
        };
        if case.helper.map(ManifestHelper::id) != expected_helper {
            return Err(format!("{} helper does not match policy", case.opcode));
        }
        let missing: Vec<_> = required_dimensions(case)
            .difference(&case.dimensions)
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(format!("{} missing {missing:?}", case.opcode));
        }
        let extra: Vec<_> = case
            .dimensions
            .difference(&required_dimensions(case))
            .copied()
            .collect();
        if !extra.is_empty() {
            return Err(format!("{} has inapplicable {extra:?}", case.opcode));
        }
    }
    Ok(())
}

#[test]
fn manifest_executes_every_advertised_opcode_at_its_native_pc() {
    let manifest: OpcodeManifest =
        serde_json::from_str(include_str!("fixtures/opcode-cases.json")).unwrap();
    validate_dimensions(&manifest).expect("manifest has semantic dimension evidence");
    for case in manifest.cases {
        // The 32-bit `goto` case needs a 6,000-statement dead block to push a
        // jump offset past 16 bits. Under sanitizer instrumentation compiling
        // that block takes over an hour on CI runners, so only the
        // uninstrumented jobs execute it; the opcode manifest gate in
        // `opcodes.rs` still requires the case to exist.
        if cfg!(rquickjs_sanitizer) && case.opcode == "goto" {
            continue;
        }
        let mut run = differential(&case.definition, &case.expression)
            .force_baseline()
            .expect_executed_opcode(&case.opcode);
        if case.dimensions.contains(&Dimension::OwnershipGc) {
            run = run.stress_gc();
        }
        // Helper annotations describe the exact generic fallback family. A
        // primitive-specialized Tier 1 execution may legitimately keep that
        // cold edge unexecuted; helper execution is covered independently by
        // `every_advertised_helper_family_has_a_real_native_execution_case`.
        run.assert_same();
    }
}

#[test]
fn manifest_dimension_schema_is_closed_and_required_dimensions_are_mechanical() {
    let source = include_str!("fixtures/opcode-cases.json");
    let manifest: OpcodeManifest = serde_json::from_str(source).unwrap();
    validate_dimensions(&manifest).unwrap();

    let mut value: serde_json::Value = serde_json::from_str(source).unwrap();
    value["cases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|case| case["opcode"] == "add")
        .unwrap()["dimensions"]
        .as_array_mut()
        .unwrap()
        .retain(|dimension| dimension != "coercion-reentrancy");
    let missing: OpcodeManifest = serde_json::from_value(value.clone()).unwrap();
    assert!(validate_dimensions(&missing).unwrap_err().contains("add"));

    value["cases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|case| case["opcode"] == "add")
        .unwrap()["dimensions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("invented-dimension"));
    assert!(serde_json::from_value::<OpcodeManifest>(value).is_err());
}

#[test]
fn rejected_programs_have_exact_fallback_and_interpreter_semantics() {
    assert_tier1_rejected(
        "function f(a){ return a ? 1n : 2n }",
        "String(f(86))",
        FallbackReason::UnsupportedOpcode,
    );
}

#[test]
fn ordinary_synchronous_programs_enter_tier1_and_match_the_interpreter() {
    let programs = [
        ("function f(a,b){ return a+b }", "f(20,22)"),
        ("function f(a){ let x=a; x++; return x }", "f(41)"),
        (
            "function f(a){ if(a) return 1; return 2 }",
            "[f(true),f(false)]",
        ),
        ("function f(o){ return o.answer }", "f({answer:42})"),
        ("function f(){ return [20,22] }", "f()"),
    ];

    for (definition, expression) in programs {
        differential(definition, expression)
            .force_baseline()
            .expect_executed_opcode(match expression {
                "f(20,22)" => "add",
                "f(41)" => "post_inc",
                "[f(true),f(false)]" => "if_false8",
                "f({answer:42})" => "get_field",
                "f()" => "array_from",
                _ => unreachable!(),
            })
            .assert_same();
    }
}

#[test]
fn atom_string_literals_enter_tier1_with_owned_values() {
    differential(
        "function f(flag){return flag?'quickjs-jit-atom':'fallback-atom'}",
        "f(true)+'|'+f(false)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("push_atom_value")
    .assert_same();
}

#[test]
fn string_valued_object_literals_enter_tier1_with_owned_fields() {
    differential(
        "function f(value){return {kind:'panel',value:value}}",
        "JSON.stringify(f(42))",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("define_field")
    .assert_same();
}

#[test]
fn computed_object_fields_enter_tier1_with_owned_container_and_key() {
    differential(
        "function f(key,value){return {[key]:value}}",
        "JSON.stringify(f('panel',42))",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("define_array_el")
    .assert_same();
}

#[test]
fn packed_array_slack_never_behaves_like_live_elements() {
    differential(
        "function f(o,k){let v=o[k];return v+0}\n\
         globalThis.a=[];for(let i=1;i<=32;i++)a.push(i);\n\
         while(a.length>2)a.pop();",
        "[f(a,2),f(a,3),f(a,8)].map(String).join(',')",
    )
    .force_baseline()
    .expect_executed_opcode("get_array_el")
    .assert_same();

    differential(
        "function f(o,k,v){o[k]=v;return o.length+0}\n\
         globalThis.a=[];for(let i=1;i<=32;i++)a.push(i);\n\
         while(a.length>2)a.pop();",
        "f(a,5,999)+'|'+a.length+'|'+String(a[5])",
    )
    .force_baseline()
    .expect_executed_opcode("put_array_el")
    .assert_same();
}

#[test]
fn tier1_calls_and_method_calls_preserve_values_and_ownership() {
    differential(
        "function f(fn,a,b){let value=fn(a,b);return value+0}",
        "f((a,b)=>a+b,20,22)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("call2")
    .expect_helper(HelperId::Call)
    .assert_same();

    differential(
        "function f(o,x){let value=o.add(x);return value+0}",
        "f({base:20,add(x){return this.base+x}},22)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("call_method")
    .expect_helper(HelperId::Call)
    .assert_same();
}

#[test]
fn tier1_global_lookup_preserves_json_exceptions_and_gc_ownership() {
    differential(
        "function f(value){let result=JSON.stringify(value);return result}",
        "f({answer:42})",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("get_var")
    .expect_helper(HelperId::GetGlobal)
    .assert_same();

    differential(
        "function f(){return missingGlobal}",
        "(()=>{try{return f()}catch(error){return error.name}})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("get_var")
    .expect_helper(HelperId::GetGlobal)
    .assert_same();
}

#[test]
fn tier1_constructor_calls_preserve_map_exceptions_and_gc_ownership() {
    differential(
        "function f(entries){let map=new Map(entries);return map.size}",
        "f([['answer',42]])",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("call_constructor")
    .expect_helper(HelperId::CallConstructor)
    .assert_same();

    differential(
        "function f(){return new Map(42)}",
        "(()=>{try{return f()}catch(error){return error.name}})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("call_constructor")
    .expect_helper(HelperId::CallConstructor)
    .assert_same();
}

#[test]
fn tier1_regexp_literal_preserves_strings_and_gc_ownership() {
    differential(
        "function f(value){let regexp=/^[a-z]+-[0-9]+$/i;let result=regexp.test(value);return result}",
        "[f('quickjs-2026'),f('not a match')]",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("regexp")
    .expect_helper(HelperId::Regexp)
    .assert_same();
}

#[test]
fn tier1_object_property_reads_and_writes_preserve_values_and_ownership() {
    differential(
        "function f(o){o.answer=42;return o.answer}",
        "f({answer:0})",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("put_field")
    .expect_executed_opcode("get_field")
    .expect_helper(HelperId::SetProperty)
    .expect_helper(HelperId::GetProperty)
    .assert_same();
}

#[test]
fn coercion_exception_and_gc_visible_ownership_match() {
    differential(
        "const events=[]; function f(a,b){ return a+b }",
        "f({[Symbol.toPrimitive](){events.push('a');return 20}}, {[Symbol.toPrimitive](){events.push('b');return 22}}) + ':' + events.join(',')",
    )
    .force_baseline()
    .expect_executed_opcode("add")
    .expect_helper(HelperId::AddSlow)
    .assert_same();

    differential(
        "function f(o){ return [o,o] }",
        "(() => { const o={x:42}; const a=f(o); return a[0]===a[1] && a[0].x })()",
    )
    .force_baseline()
    .assert_same();
}

#[test]
fn trusted_observer_is_installed_before_untrusted_definition_poisoning() {
    differential(
        "Number.isNaN=()=>{throw 1};Object.is=()=>{throw 2};Symbol.keyFor=()=>{throw 3};String=()=>{throw 4};JSON.stringify=()=>{throw 5};function f(a,b){return a+b}",
        "f(NaN,0)",
    )
    .force_baseline()
    .expect_executed_opcode("add")
    .assert_same();
}

#[test]
fn every_advertised_helper_family_has_a_real_native_execution_case() {
    let cases = [
        (
            "function f(a){ return +a }",
            "f({valueOf(){return 42}})",
            "plus",
            HelperId::ToNumeric,
        ),
        (
            "function f(a,b){ return a < b }",
            "f({valueOf(){return 20}},22)",
            "lt",
            HelperId::CompareSlow,
        ),
        (
            "function f(a,b){ return a % b }",
            "f({valueOf(){return 20}},6)",
            "mod",
            HelperId::BinaryArithSlow,
        ),
        (
            "function f(a){ return -a }",
            "f({valueOf(){return 20}})",
            "neg",
            HelperId::UnaryArithSlow,
        ),
        (
            "function f(o,a){ o.answer=a; return o.answer }",
            "f({},42)",
            "put_field",
            HelperId::SetProperty,
        ),
        (
            "function f(g,a){ let x=g(a); return x+0 }",
            "f(x=>x+1,41)",
            "call1",
            HelperId::Call,
        ),
        (
            "function f(entries){let map=new Map(entries);return map.size}",
            "f([['answer',42]])",
            "call_constructor",
            HelperId::CallConstructor,
        ),
        (
            "function f(value){let regexp=/^[a-z]+-[0-9]+$/i;let result=regexp.test(value);return result}",
            "f('quickjs-2026')",
            "regexp",
            HelperId::Regexp,
        ),
        (
            "function f(){ return {} }",
            "f()",
            "object",
            HelperId::NewObject,
        ),
        (
            "function f(a){ const g = () => a + 1; return g() }",
            "f(41)",
            "fclosure8",
            HelperId::FClosure,
        ),
        (
            "function f(a){ const g = () => a + 1; return g.name + g() }",
            "f(41)",
            "set_name",
            HelperId::SetName,
        ),
        (
            "var f = (function(){ var c = 40; return function f(a){ return c + a } })()",
            "f(2)",
            "get_var_ref0",
            HelperId::GetVarRef,
        ),
        (
            "var f = (function(){ var c = 40; return function f(a){ c = a; return 1 } })()",
            "f(2)",
            "put_var_ref0",
            HelperId::PutVarRef,
        ),
        (
            "function f(n){ const fs = []; for (let i = 0; i < n; i++) { fs.push(() => i) } let s = 0; for (let j = 0; j < n; j++) { const g = fs[j]; s += g() } return s }",
            "f(5)",
            "close_loc",
            HelperId::CloseLocal,
        ),
        (
            "function f(values){ let sum=0; for(const value of values) sum+=value; return sum }",
            "f(new Set([20,22]))",
            "for_of_next",
            HelperId::IteratorOp,
        ),
    ];
    for (definition, expression, opcode, helper) in cases {
        differential(definition, expression)
            .force_baseline()
            .expect_executed_opcode(opcode)
            .expect_helper(helper)
            .assert_same();
    }
}

#[test]
fn seeded_structured_programs_match_interpreter_and_automatic_modes() {
    for seed in 0..64 {
        let (definition, invocation) = seeded_eligible_function(seed);
        differential(definition, &invocation)
            .force_baseline()
            .expect_executed_opcode(match seed % 3 {
                0 => "add",
                1 => "post_inc",
                _ => "if_false8",
            })
            .assert_same();
        let interpreter = Runtime::new().unwrap();
        let expected = Context::full(&interpreter).unwrap().with(|ctx| {
            ctx.eval::<(), _>(canonical_observer_prelude()).unwrap();
            ctx.eval::<(), _>(definition).unwrap();
            ctx.eval::<String, _>(canonical_observation_source(&invocation))
                .unwrap()
        });
        let automatic = JitRuntime::builder()
            .config(
                rquickjs_jit::JitConfig::builder()
                    .call_threshold(1)
                    .loop_threshold(1)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let automatic_context = Context::full(&automatic).unwrap();
        automatic_context.with(|ctx| ctx.eval::<(), _>(canonical_observer_prelude()).unwrap());
        automatic_context.with(|ctx| ctx.eval::<(), _>(definition).unwrap());
        install_warm_loop(&automatic_context, &invocation);
        let deadline = std::time::Instant::now() + native_evidence_budget();
        while std::time::Instant::now() < deadline {
            run_warm_loop(&automatic_context);
            automatic.jit().poll();
            if automatic.metrics().native_entries > 0 {
                break;
            }
        }
        let actual = automatic_context.with(|ctx| {
            ctx.eval::<String, _>(canonical_observation_source(&invocation))
                .unwrap()
        });
        let metrics = automatic.metrics();
        assert!(
            metrics.native_entries > 0,
            "seed {seed} never entered automatic native code: {metrics:?}"
        );
        assert_eq!(
            metrics.native_fallbacks, 0,
            "seed {seed} automatic fallback: {metrics:?}"
        );
        assert_eq!(
            metrics.native_retries, 0,
            "seed {seed} automatic retry: {metrics:?}"
        );
        assert!(
            metrics.installed > 0,
            "seed {seed} was not installed: {metrics:?}"
        );
        assert_eq!(
            actual, expected,
            "seed {seed}; definition={definition}; invocation={invocation}"
        );
    }
}

#[test]
fn seeded_structured_programs_enter_optimized_mode_with_native_evidence() {
    for seed in 0..16 {
        let (definition, invocation) = seeded_eligible_function(seed);
        let interpreter = Runtime::new().unwrap();
        let expected = Context::full(&interpreter).unwrap().with(|ctx| {
            ctx.eval::<(), _>(canonical_observer_prelude()).unwrap();
            ctx.eval::<(), _>(definition).unwrap();
            ctx.eval::<String, _>(canonical_observation_source(&invocation))
                .unwrap()
        });
        let config = rquickjs_jit::JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .build()
            .unwrap();
        let optimized = JitRuntime::builder().config(config).build().unwrap();
        let context = Context::full(&optimized).unwrap();
        context.with(|ctx| ctx.eval::<(), _>(canonical_observer_prelude()).unwrap());
        context.with(|ctx| ctx.eval::<(), _>(definition).unwrap());
        install_warm_loop(&context, &invocation);
        let deadline = std::time::Instant::now() + native_evidence_budget();
        while std::time::Instant::now() < deadline {
            run_warm_loop(&context);
            optimized.jit().poll();
            if optimized.metrics().tier2_entries > 0 {
                break;
            }
        }
        let actual = context.with(|ctx| {
            ctx.eval::<String, _>(canonical_observation_source(&invocation))
                .unwrap()
        });
        let metrics = optimized.metrics();
        assert!(
            metrics.tier2_entries > 0,
            "seed {seed} never entered Tier2: {metrics:?}"
        );
        assert_eq!(
            metrics.native_fallbacks, 0,
            "seed {seed} optimized fallback: {metrics:?}"
        );
        assert_eq!(
            metrics.native_retries, 0,
            "seed {seed} optimized retry: {metrics:?}"
        );
        assert!(
            metrics.installed > 0 && metrics.native_entries > 0,
            "seed {seed} lacks native evidence: {metrics:?}"
        );
        assert_eq!(actual, expected, "optimized seed {seed}");
    }
}

/// Time a seed may take to reach native code. Sanitizer builds compile far
/// more slowly, and this file's other tests share the runner's cores.
fn native_evidence_budget() -> std::time::Duration {
    std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 120 } else { 10 })
}

/// Warm through one function defined once. Evaluating a fresh warm script
/// per iteration creates a new (now Tier 1 eligible) function every time,
/// and with `call_threshold(1)` those throwaway scripts fill the compile
/// queue ahead of the function under test.
fn install_warm_loop(context: &Context, invocation: &str) {
    let warm = format!("globalThis.warm=function(){{for(let i=0;i<256;i++){{{invocation};}}}}");
    context.with(|ctx| ctx.eval::<(), _>(warm.as_str()).unwrap());
}

fn run_warm_loop(context: &Context) {
    context.with(|ctx| {
        let warm: Function<'_> = ctx.globals().get("warm").unwrap();
        warm.call::<_, ()>(()).unwrap();
    });
}

fn seeded_eligible_function(seed: u64) -> (&'static str, String) {
    let a = (seed as i16 % 97) - 48;
    let b = ((seed.rotate_left(17) as i16) % 31) - 15;
    let definition = match seed % 3 {
        0 => "function f(a,b){return a+b}",
        1 => "function f(a,b){let x=a+b;x++;return x}",
        _ => "function f(a,b){let x=a+b;if(x)return x;return b}",
    };
    (definition, format!("f({a},{b})"))
}
