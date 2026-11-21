//! Tier 1 exception regions: `throw`, `throw_error`, try/catch (`catch`,
//! `nip_catch`) and try/finally (`gosub`, `ret`) execute natively and match
//! the interpreter exactly, including exceptions raised by helpers inside a
//! try region, nested regions, rethrow, and uncatchable interrupts.

#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use rquickjs::{Context, Runtime};
use rquickjs_jit::test_support::{differential, forced_baseline};
use rquickjs_jit::{Jit, JitConfig, JitMetrics, JitTierPolicy};

fn same(definition: &str, expression: &str, opcode: &str) {
    differential(definition, expression)
        .force_baseline()
        .expect_executed_opcode(opcode)
        .assert_same();
    differential(definition, expression)
        .force_baseline()
        .expect_executed_opcode(opcode)
        .stress_gc()
        .assert_same();
}

#[test]
fn catch_in_a_hot_loop_resumes_the_handler_natively() {
    // `neg` occurs only in the handler: it must execute in native code.
    same(
        "function f(n){ let caught=0; for(let i=0;i<n;i++){ try { if((i&15)===0) throw i; } catch (value) { caught-=-value; } } return caught; }",
        "f(160)",
        "neg",
    );
    same(
        "function f(n){ let caught=0; for(let i=0;i<n;i++){ try { if((i&15)===0) throw i; } catch (value) { caught+=value; } } return caught; }",
        "f(160)",
        "throw",
    );
}

#[test]
fn uncaught_throw_leaves_with_the_exact_exception() {
    same(
        "function f(a){ if (a > 1) throw {code: a}; return a; }",
        "(function(){ try { return f(3); } catch (e) { return 'outer:' + e.code; } })()",
        "throw",
    );
    same(
        "function f(a){ throw new TypeError('bad ' + a); }",
        "(function(){ try { f(7); } catch (e) { return e.name + ':' + e.message; } })()",
        "throw",
    );
}

#[test]
fn helper_exceptions_inside_try_reach_the_native_handler() {
    // GET_PROPERTY on undefined, CALL of a non-function and a throwing getter.
    same(
        "function f(o){ let r = 0; try { r = o.x.y; } catch (e) { r = -1; } return r; }",
        "[f({x:{y:5}}), f({}), f({x:null})].join()",
        "push_minus1",
    );
    same(
        "function f(g){ try { return g(1); } catch (e) { return 'caught:' + e.name; } }",
        "[f(function(v){return v+1}), f(5), f(null)].join()",
        "nip_catch",
    );
    same(
        "function f(o){ let n = 0; for (let i = 0; i < 20; i++) { try { n += o.v; } catch (e) { n -= -e; } } return n; }",
        "f({ get v() { if (this.k++ % 3 === 0) throw 7; return 2; }, k: 0 })",
        "neg",
    );
    // Pending owned operands above the catch offset are released exactly.
    same(
        "function f(a, o){ let r; try { r = [a, {k: a}, o.x.y]; } catch (e) { r = -1; } return r; }",
        "String(f({}, {})) + String(f([1], {x: {y: 2}}).length)",
        "push_minus1",
    );
}

#[test]
fn error_stack_is_captured_at_the_throwing_pc() {
    same(
        "function f(o){ try { return o.missing.field; } catch (e) { return e.stack; } }",
        "f({})",
        "catch",
    );
    same(
        "function f(a){\n  try {\n    throw new Error('line' + a);\n  } catch (e) {\n    return e.stack;\n  }\n}",
        "f(2)",
        "throw",
    );
}

#[test]
fn observable_backtrace_hooks_run_for_natively_caught_primitives() {
    // The interpreter builds a backtrace for every caught primitive. It is
    // skipped natively only when building it cannot run user code; these
    // hooks make it observable and must fire exactly as in the interpreter.
    same(
        "var calls = 0; Error.prepareStackTrace = function(e, s) { calls++; return 'trace'; };\nfunction f(n){ let c = 0; for (let i = 0; i < n; i++) { try { if (i % 4 === 0) throw i; } catch (v) { c -= -v; } } return c + ':' + calls; }",
        "f(40)",
        "neg",
    );
    same(
        "var calls = 0; Error.stackTraceLimit = { valueOf() { calls++; return 3; } };\nfunction f(n){ let c = 0; for (let i = 0; i < n; i++) { try { if (i % 4 === 0) throw {i}; } catch (v) { c -= -v.i; } } return c + ':' + calls; }",
        "f(40)",
        "neg",
    );
}

#[test]
fn rethrow_and_nested_regions_match_the_interpreter() {
    same(
        "function f(a){ let log = ''; try { try { throw a; } catch (e) { log += 'inner' + e; throw e + 1; } } catch (e) { log += ',outer' + e; } return log; }",
        "f(1)",
        "catch",
    );
    same(
        "function f(a){ let log = ''; try { try { throw a; } finally { log += 'finally'; } } catch (e) { log += ',caught' + e; } return log; }",
        "f(4)",
        "gosub",
    );
    same(
        "function f(a){ let log = ''; try { try { log += 'body'; } finally { log += ',f1'; } log += ',after'; } finally { log += ',f2'; } return log; }",
        "f(0)",
        "ret",
    );
    same(
        "function f(a){ try { return a + 1; } finally { a = 100; } }",
        "f(41)",
        "gosub",
    );
    same(
        "function f(a){ try { throw a; } finally { return 'override'; } }",
        "f(1)",
        "throw",
    );
    same(
        "function f(a){ let log = []; try { log.push('t'); throw new RangeError('r' + a); } catch (e) { log.push(e.message); } finally { log.push('f'); } return log.join(); }",
        "f(9)",
        "gosub",
    );
}

#[test]
fn control_transfers_through_finally_blocks() {
    same(
        "function f(n){ let c = 0; for (let i = 0; i < n; i++) { try { if (i % 3 === 0) continue; c += i; } finally { c += 1000; } } return c; }",
        "f(20)",
        "ret",
    );
    same(
        "function f(n){ let c = 0; while (true) { try { c++; if (c > n) break; } finally { c += 10; } } return c; }",
        "f(35)",
        "gosub",
    );
}

#[test]
fn throw_error_raises_the_interpreter_error() {
    same(
        "function f(a){ const k = a; try { k = 2; } catch (e) { return e.name + ':' + e.message; } return 'none'; }",
        "f(1)",
        "throw_error",
    );
    same(
        "function f(a){ const k = a; k = 2; }",
        "(function(){ try { f(1); } catch (e) { return e.name + ':' + e.message; } })()",
        "throw_error",
    );
}

/// Runs `source` then evaluates `expression` `rounds` times with and without
/// the production JIT, returning both results and the JIT metrics.
fn production(source: &str, expression: &str, rounds: usize) -> (String, String, JitMetrics) {
    production_with(JitConfig::default(), source, expression, rounds)
}

fn production_with(
    config: JitConfig,
    source: &str,
    expression: &str,
    rounds: usize,
) -> (String, String, JitMetrics) {
    let run = |jit: bool| {
        let runtime = Runtime::new().unwrap();
        let attached = jit.then(|| Jit::attach(&runtime, config.clone()).unwrap());
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| ctx.eval::<(), _>(source).unwrap());
        // Slow (sanitizer) builds may still be compiling after `rounds`; keep
        // evaluating until the queued compilation has been installed.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 600 } else { 120 });
        let mut round = 0;
        let result = loop {
            let result = context.with(|ctx| ctx.eval::<String, _>(expression).unwrap());
            while runtime.is_job_pending() {
                runtime.execute_pending_job().unwrap();
            }
            if let Some(jit) = &attached {
                jit.poll();
            }
            round += 1;
            let compiling = attached
                .as_ref()
                .is_some_and(|jit| jit.metrics().pending_worker_jobs > 0);
            if round >= rounds && (!compiling || std::time::Instant::now() >= deadline) {
                break result;
            }
        };
        (
            result,
            attached.map(|jit| jit.metrics()).unwrap_or_default(),
        )
    };
    let (interpreted, _) = run(false);
    let (compiled, metrics) = run(true);
    (interpreted, compiled, metrics)
}

const HOT_EXCEPTION_LOOP: &str = "function f(n){ let caught=0, finals=0; for(let i=0;i<n;i++){ try { if((i&15)===0) throw i; caught-=1; } catch (value) { caught+=value; } finally { finals++; } } return caught + ':' + finals; }";

fn baseline_only() -> JitConfig {
    JitConfig::builder()
        .tier_policy(JitTierPolicy::BaselineOnly)
        .build()
        .unwrap()
}

#[test]
fn baseline_policy_runs_hot_exception_loops_natively() {
    let (interpreted, compiled, metrics) =
        production_with(baseline_only(), HOT_EXCEPTION_LOOP, "f(4000)", 40);
    assert_eq!(compiled, interpreted);
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.native_retries, 0, "{metrics:?}");
    assert_eq!(metrics.invalid_artifacts, 0, "{metrics:?}");
}

#[test]
fn automatic_tiering_keeps_exception_regions_interpreted() {
    // Automatic tiering defers the opcodes Tier 1 gained in roadmap P2 until
    // profitability can measure the interpreter; explicit policies still
    // compile them (above).
    let (interpreted, compiled, metrics) = production(HOT_EXCEPTION_LOOP, "f(4000)", 40);
    assert_eq!(compiled, interpreted);
    assert_eq!(metrics.native_entries, 0, "{metrics:?}");
    assert!(metrics.tier1_rejections > 0, "{metrics:?}");
}

#[test]
fn osr_enters_a_loop_header_inside_a_try_region() {
    // The first invocation is long enough to enter the loop through OSR with
    // the catch offset live on the interpreter operand stack.
    let (interpreted, compiled, metrics) = production_with(
        baseline_only(),
        "function f(n){ let s = 0; try { for (let i = 0; i < n; i++) { s = (s + i) | 0; if (i === n - 3) throw s; } } catch (e) { return 'caught:' + e; } return 'done:' + s; }",
        "f(3000000)",
        1,
    );
    assert_eq!(compiled, interpreted);
    assert!(metrics.osr_entries >= 1, "{metrics:?}");
    assert_eq!(metrics.native_retries, 0, "{metrics:?}");
}

#[test]
fn async_functions_stop_requesting_refused_snapshots() {
    // Async and generator frames can never compile. Their refusal is local to
    // the bytecode, so hot probes must stop instead of re-requesting a
    // snapshot at every call, resume and loop poll.
    let (interpreted, compiled, metrics) = production(
        "async function step(v){ return (await Promise.resolve(v + 1)) * 3; }\n\
         async function g(n){ let s = 0; for (let i = 0; i < n; i++) { try { if ((i & 15) === 0) throw i; } catch (e) { s += e; } } for (let i = 0; i < 8; i++) s = await step(s & 0xffff); globalThis.out = String(s); }\n\
         function* gen(n){ for (let i = 0; i < n; i++) yield i; }\n\
         function h(){ let t = 0; for (const v of gen(500)) t += v; return t; }",
        "(g(2000), String(h()))",
        60,
    );
    assert_eq!(compiled, interpreted);
    assert!(
        metrics.snapshot_requests <= 8,
        "refused async/generator snapshots were re-requested: {metrics:?}"
    );
}

#[test]
fn interrupts_inside_try_regions_stay_uncatchable() {
    forced_baseline(
        "function f(n){ let c = 0; for (;;) { try { c++; } catch (e) { c--; } } } f(1)",
    )
    .interrupt_after(3)
    .assert_uncatchable_interrupt();
    forced_baseline("function f(n){ let c = 0; for (;;) { try { c++; } finally { c--; } } } f(1)")
        .interrupt_after(3)
        .assert_uncatchable_interrupt();
}

#[test]
fn handlers_without_helper_edges_still_compile_natively() {
    // QuickJS wraps a catch body in an inner region whose only instruction
    // is its own rethrow. Nothing in that region can raise, so its landing
    // pad and handler are unreachable native code and Cranelift removes
    // their safepoints; the function must still compile.
    same(
        "function f(o){ try { return o.x.y; } catch (e) { return -1; } }",
        "[f({x:{y:5}}), f({}), f({x:null})].join()",
        "catch",
    );
    same(
        "function f(a){ let r = 0; try { r = a + 1; } catch (e) { r = -2; } finally { r *= 3; } return r; }",
        "f(4)",
        "gosub",
    );
}

#[test]
fn refcounted_local_stores_inside_try_regions_keep_exact_ownership() {
    // Every store releases the previous heap value through FREE inside the
    // region; the handler then reads and overwrites the same locals.
    same(
        "function f(n, o){ let a = {v: 0}, b = [n]; for (let i = 0; i < n; i++) { try { a = {v: i}; b = [a, i]; if (i % 3 === 0) a = o.missing.x; } catch (e) { b = [b, a.v]; a = {v: -i}; } } return a.v + ':' + b.length; }",
        "f(12, {})",
        "catch",
    );
}

#[test]
fn throws_from_handlers_without_an_enclosing_region_leave_exactly() {
    same(
        "function f(n){ try { throw 'a'; } catch (e) { throw e + n; } }",
        "(function(){ try { f(3); } catch (e) { return 'outer:' + e; } })()",
        "throw",
    );
    // Recursive catch and rethrow across native frames.
    same(
        "function f(n){ if (n <= 0) throw 'bottom'; try { return f(n - 1) + 1; } catch (e) { throw e + n; } }",
        "(function(){ try { return f(6); } catch (e) { return 'outer:' + e; } })()",
        "throw",
    );
}

#[test]
fn production_tiering_installs_common_try_catch_shapes() {
    // Tier 1 only: every compile outcome counted here is a Tier 1 outcome.
    let (interpreted, compiled, metrics) = production_with(
        JitConfig::builder()
            .tier_policy(JitTierPolicy::BaselineOnly)
            .build()
            .unwrap(),
        "function f(n){ if (n <= 0) return 0; try { return f(n - 1) + 1; } catch (e) { return -1; } }\n\
         function g(o){ try { return o.x.y; } catch (e) { return -1; } }\n\
         function run(){ let t = 0; for (let i = 0; i < 400; i++) { t += f(i & 7); t += g((i & 7) ? {x:{y:i}} : {}); } return String(t); }",
        "run()",
        40,
    );
    assert_eq!(compiled, interpreted);
    // f, g and run all install; none fails as an invalid artifact.
    assert!(metrics.installed >= 3, "{metrics:?}");
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert_eq!(metrics.invalid_artifacts, 0, "{metrics:?}");
}

#[test]
fn handlers_resume_with_locals_written_by_a_throwing_closure() {
    // P2a x P2c: `fclosure` makes the var refs alias this frame's local
    // slots. The throwing call runs the closure, which replaces `x` (and
    // releases its old object) through the var ref before throwing. The
    // landing pad must resume the handler from the frame, not from the
    // native copy published before the call.
    same(
        "function f(n){ let x = {a: -1}; const set = (v) => { x = v; throw 0; }; try { set({a: n}); } catch (e) { x.a += 1; } return x.a; }",
        "[f(1), f(41)].join()",
        "catch",
    );
    // Same through a finally block and a helper-raised TypeError, with the
    // written local read both in the handler and after the region.
    same(
        "function f(n){ let x = 'old'; const g = () => { x = 'new' + n; return null.p; }; let r = ''; try { g(); } catch (e) { r = x; } finally { r += ':' + x; } return r + ':' + x; }",
        "[f(1), f(2)].join()",
        "catch",
    );
    // The catch prologue's own backtrace hook runs user code that writes a
    // captured local before the handler resumes.
    same(
        "function f(n){ let seen = 0; Error.prepareStackTrace = () => { seen = n; return 's'; }; const probe = () => seen; try { null.p; } catch (e) { e.stack; } finally { Error.prepareStackTrace = undefined; } return probe() + seen; }",
        "[f(3), f(4)].join()",
        "catch",
    );
}
