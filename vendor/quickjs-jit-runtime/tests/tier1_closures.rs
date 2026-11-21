//! Tier 1 closure coverage: `fclosure`, closure-variable access, `close_loc`
//! and `set_name`. Every program runs once in the interpreter and once with
//! the named function `f` forced into published Tier 1 code; results must be
//! identical, including exceptions, TDZ errors and values that a closure
//! writes into the native frame's captured arguments and locals.
#![cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use rquickjs::{Context, Runtime};
use rquickjs_jit::bytecode::{FallbackReason, HelperId};
use rquickjs_jit::test_support::{assert_tier1_rejected, differential};
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::time::{Duration, Instant};

#[test]
fn closure_writes_to_captured_locals_are_reloaded_after_every_call() {
    // `inc` writes the captured `c` through its attached var ref while the
    // native frame holds a register copy. The following `c++` must observe it.
    differential(
        "function f(n){ let c = 0; const inc = () => { c += 2 }; for (let i = 0; i < n; i++) { inc(); c++ } return c }",
        "f(10)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("fclosure8")
    .expect_helper(HelperId::FClosure)
    .assert_same();
}

#[test]
fn closure_writes_to_captured_arguments_are_reloaded() {
    differential(
        "function f(a){ const set = v => { a = v }; set(5); let r = a + 1; set('x'); return r + ':' + a }",
        "f(1)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("fclosure8")
    .assert_same();
}

#[test]
fn closure_type_change_takes_the_native_numeric_guard_exit() {
    // The closure turns the captured Int32 into a string. Subtraction is a
    // native-only numeric operation, so the reloaded value must fail its
    // tag guard and deoptimize instead of using the stale register copy.
    differential(
        "function f(n){ let x = n; const g = () => { x = 'a' + x }; g(); return x - 1 }",
        "String(f(3))",
    )
    .force_baseline()
    .expect_executed_opcode("fclosure8")
    .expect_deopt()
    .assert_same();
}

#[test]
fn captured_heap_values_keep_exact_ownership_under_stress_gc() {
    differential(
        "function f(n){ let o = { v: 0 }; const g = () => { o = { v: o.v + 1, prev: o } }; for (let i = 0; i < n; i++) g(); return o.v + ':' + o.prev.v }",
        "f(20)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("fclosure8")
    .assert_same();
}

#[test]
fn per_iteration_bindings_are_closed_at_loop_boundaries() {
    differential(
        "function f(n){ const fs = []; for (let i = 0; i < n; i++) { fs.push(() => i * 10) } let s = ''; for (let j = 0; j < n; j++) { const g = fs[j]; s += g() + ',' } return s }",
        "f(6)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("close_loc")
    .expect_helper(HelperId::CloseLocal)
    .assert_same();
}

#[test]
fn closure_variable_reads_raise_the_interpreter_tdz_error() {
    differential(
        "var f = (function(){ if (globalThis) return function f(){ return x }; let x = 1 })()",
        "(() => { try { return f() } catch (e) { return e.name + ':' + e.message } })()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("get_var_ref_check")
    .expect_helper(HelperId::GetVarRef)
    .assert_same();
}

#[test]
fn closure_variable_writes_raise_the_interpreter_tdz_error() {
    differential(
        "var f = (function(){ if (globalThis) return function f(v){ x = v; return 1 }; let x = 1 })()",
        "(() => { try { return f({}) } catch (e) { return e.name + ':' + e.message } })()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("put_var_ref_check")
    .expect_helper(HelperId::PutVarRef)
    .assert_same();
}

#[test]
fn closure_variables_are_shared_between_closures_and_the_owner() {
    differential(
        "var peek; var f = (function(){ let c = 1; peek = () => c; return function f(a){ c = c * 2 + a; return c + peek() } })()",
        "[f(1), f(2), peek()].join()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("put_var_ref_check")
    .assert_same();
}

#[test]
fn set_var_ref_keeps_the_stored_heap_value_on_the_stack() {
    differential(
        "var peek; var f = (function(){ var c = null; peek = () => c; return function f(a){ return (c = { a: a }).a + peek().a } })()",
        "f(21)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("set_var_ref0")
    .assert_same();
}

#[test]
fn set_var_ref_keep_duplicates_heap_values_with_inline_refcounting() {
    // Integration of P2a closures with P1b inline refcounting: `set_var_ref`
    // keeps its value by duplicating it into the scratch slot. Outside stress
    // GC that duplicate is a native `ref_count++`, not the DUP helper, and
    // the dropped operand is a shared reference (the var ref still owns the
    // object), so it is released inline too. A missing or doubled count would
    // free the object that `peek` later reads.
    differential(
        "var peek; var f = (function(){ var c = null; peek = () => c; return function f(a){ return (c = { a: a }).a } })()",
        "(() => { let s = 0; for (let i = 0; i < 40; i++) s += f(i) + peek().a; return s + ':' + (peek() === peek()) + ':' + peek().a })()",
    )
    .force_baseline()
    .expect_executed_opcode("set_var_ref0")
    .expect_helper(HelperId::PutVarRef)
    .expect_ownership_helper_counts(0, 0)
    .assert_same();
}

#[test]
fn captured_heap_locals_released_inline_are_reloaded_after_closure_writes() {
    // P1b releases shared heap references inline (writing `undefined` to the
    // frame slot) while P2a reloads captured locals after every helper. The
    // closure replaces the captured object each iteration; the owner then
    // overwrites the local too. The native copy must follow both writers and
    // the refcounts must stay exact without stress-GC helpers.
    differential(
        "function f(n){ let o = { v: 0 }; const keep = []; const g = () => { o = { v: o.v + 1 } }; for (let i = 0; i < n; i++) { g(); keep.push(o); o = { v: o.v * 2 } } return o.v + ':' + keep.length + ':' + keep[n - 1].v }",
        "f(12)",
    )
    .force_baseline()
    .expect_executed_opcode("fclosure8")
    .assert_same();
}

#[test]
fn anonymous_closure_names_match_the_interpreter() {
    differential(
        "function f(){ const g = () => 1; const h = function(){ return 2 }; return g.name + ',' + h.name + ',' + (g() + h()) }",
        "f()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("set_name")
    .expect_helper(HelperId::SetName)
    .assert_same();
}

#[test]
fn generator_and_async_closures_are_created_but_run_in_the_interpreter() {
    differential(
        "function f(n){ const gen = function* () { yield n; yield n + 1 }; const it = gen(); const a = it.next().value; const b = it.next().value; return a + b }",
        "f(20)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("fclosure8")
    .assert_same();
}

#[test]
fn derived_constructor_this_initialization_through_arrows_stays_rejected() {
    assert_tier1_rejected(
        "var f = (function(){ let r; class A { constructor(){ this.v = 1 } } class C extends A { constructor(){ r = () => super(); r() } } new C(); return r })()",
        "(() => { try { f(); return 'no' } catch (e) { return e.name } })()",
        FallbackReason::ClosureFrame,
    );
}

#[test]
fn baseline_only_tiering_runs_closure_creating_loops_natively() {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .call_threshold(1)
            .loop_threshold(1)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(
            "globalThis.kernel = function kernel(n){ let c = 0; const add = x => y => (x + y) | 0; for (let i = 0; i < n; i++) { c = add(c)(i & 31) } return c }",
        )
        .unwrap()
    });
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    let mut result = 0;
    while Instant::now() < deadline {
        result = context.with(|ctx| ctx.eval::<i32, _>("kernel(200)").unwrap());
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    let expected = (0..200).fold(0_i32, |c, i| c.wrapping_add(i & 31));
    assert_eq!(result, expected);
    assert!(jit.metrics().native_entries > 0, "{:?}", jit.metrics());
    let after = context.with(|ctx| ctx.eval::<i32, _>("kernel(200)").unwrap());
    assert_eq!(after, expected);
}

#[test]
fn closure_writes_during_getters_and_value_of_are_reloaded() {
    // The closures write the captured `c` from inside GET_PROPERTY (an
    // accessor), ADD_SLOW and COMPARE_SLOW (`valueOf`), not only from CALL.
    // Each write also changes the slot's type, so the native copy must be
    // reloaded after every one of those helpers.
    differential(
        "function f(n){ let c = 0; const o = {}; Object.defineProperty(o, 'p', { get: () => { c = c + 0.5; return 1 } }); const v = { valueOf: () => { c = 'v' + c; return 2 } }; let s = 0; for (let i = 0; i < n; i++) { s += o.p; c++ } const t = s + v; const lt = s < v; c += '!'; return t + ':' + lt + ':' + c }",
        "f(6)",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("get_field")
    .expect_helper(HelperId::GetProperty)
    .assert_same();
}

#[test]
fn closure_tdz_write_error_unwinds_live_operands_to_the_callers_handler() {
    // `x = v` raises the TDZ ReferenceError from PUT_VAR_REF while the array,
    // its `concat` method and `v` are live operands. The native frame must
    // release exactly those values and let the interpreter caller catch the
    // error; stress GC detects a missing or doubled release.
    differential(
        "var f = (function(){ if (globalThis) return function f(v){ const r = [1, 2]; return r.concat(v, (x = v, 3)).length }; let x = 1 })()",
        "(() => { const out = []; for (let i = 0; i < 3; i++) { try { out.push(f({ i })) } catch (e) { out.push(e.name + ':' + e.message) } } return out.join('|') })()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("put_var_ref_check")
    .expect_helper(HelperId::PutVarRef)
    .assert_same();
}

fn run_until_native(jit: &Jit, context: &Context, source: &str) -> (i32, u64) {
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    let mut result = 0;
    while Instant::now() < deadline {
        result = context.with(|ctx| ctx.eval::<i32, _>(source).unwrap());
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    (result, jit.metrics().native_entries)
}

#[test]
fn automatic_tiering_keeps_closure_creating_functions_in_the_interpreter() {
    // Tier 2 rejects closure opcodes, so a closure-creating function would be
    // stuck in the helper-bridge Tier 1, which measured slower than the
    // interpreter on `calls-recursion-closures`. Production automatic tiering
    // therefore leaves it in the interpreter, while an explicit Tier 1 policy
    // still compiles it. The closure is never called, so no other function can
    // account for a native entry.
    let kernel = "globalThis.kernel = function kernel(n){ let s = 0; let keep = null; for (let i = 0; i < n; i++) { keep = () => s; s = (s + i) | 0 } return keep === null ? -1 : s }";
    let expected = (0..200).fold(0_i32, |s, i| s.wrapping_add(i));
    for (policy, native) in [
        (JitTierPolicy::Automatic, false),
        (JitTierPolicy::BaselineOnly, true),
    ] {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .tier_policy(policy)
                .call_threshold(1)
                .loop_threshold(1)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| ctx.eval::<(), _>(kernel).unwrap());
        let (result, entries) = if native {
            run_until_native(&jit, &context, "kernel(200)")
        } else {
            let mut result = 0;
            for _ in 0..200 {
                result = context.with(|ctx| ctx.eval::<i32, _>("kernel(200)").unwrap());
                jit.poll();
            }
            (result, jit.metrics().native_entries)
        };
        assert_eq!(result, expected, "{policy:?}");
        let metrics = jit.metrics();
        if native {
            assert!(entries > 0, "{metrics:?}");
        } else {
            assert_eq!(entries, 0, "{metrics:?}");
            assert_eq!(metrics.osr_entries, 0, "{metrics:?}");
            assert!(metrics.tier1_rejections >= 1, "{metrics:?}");
        }
    }
}

#[test]
fn osr_into_a_closure_creating_loop_observes_closure_type_changes() {
    // A long first invocation enters the Tier 1 loop through OSR. The
    // function creates closures but captures nothing itself
    // (`closure_count == 0`), so OSR is allowed. Late in the loop the closure
    // turns the captured Int32 into a Float64 and then into a string; the OSR
    // code must reload the slot after the call and keep the interpreter's
    // result.
    let source = "function f(n){ let x = 0; const bump = v => { x = v }; let s = 0; for (let i = 0; i < n; i++) { if (i === n - 1000) bump(x + 0.5); else if (i === n - 10) bump('s'); s = (s + i) | 0; x = x + 1 } return s + ':' + x } f(3000000)";
    let expected = {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| ctx.eval::<String, _>(source).unwrap())
    };
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    let actual = context.with(|ctx| ctx.eval::<String, _>(source).unwrap());
    assert_eq!(actual, expected);
    assert!(expected.ends_with("s1111111111"), "{expected}");
    assert!(jit.metrics().osr_entries >= 1, "{:?}", jit.metrics());
}
