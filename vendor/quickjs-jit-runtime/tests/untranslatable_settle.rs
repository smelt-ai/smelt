#![cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

//! Automatic tiering for Tier 1 functions that Tier 2 can never translate.

fn automatic_runtime() -> (rquickjs::Runtime, rquickjs_jit::Jit, rquickjs::Context) {
    let runtime = rquickjs::Runtime::new().unwrap();
    let jit = rquickjs_jit::Jit::attach(
        &runtime,
        rquickjs_jit::JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = rquickjs::Context::full(&runtime).unwrap();
    (runtime, jit, context)
}

#[test]
fn automatic_tiering_keeps_typeof_loops_interpreted() {
    // `typeof` is one of the opcodes Tier 1 gained in roadmap P2. Automatic
    // tiering defers them until profitability can measure the interpreter, so
    // this loop stays interpreted as it did at 0.12.9 and never reaches the
    // untranslatable settle.
    let metrics = interpreted_after_rounds(
        "globalThis.kinds = function(items) { let n = 0; \
           for (let i = 0; i < items.length; i++) { if (typeof items[i] === 'string') n++; } \
           return '' + n; }; \
         globalThis.items = ['a', 1, 'b', {}, 'c'];",
        "kinds(items)",
        "3",
    );
    assert_eq!(metrics.profitability_evaluations, 0, "{metrics:?}");
}

#[test]
fn automatic_tiering_keeps_this_methods_interpreted() {
    // `push_this` is deferred with the other P2 opcodes, so a call-only method
    // is rejected before it can reach the generic-call demotion.
    let metrics = interpreted_after_rounds(
        "function Box(v) { this.v = v; } \
         Box.prototype.twice = function() { return this.add(this.v); }; \
         Box.prototype.add = function(x) { return this.v + x; }; \
         globalThis.box = new Box(21);",
        "'' + box.twice()",
        "42",
    );
    assert_eq!(metrics.generic_call_rejections, 0, "{metrics:?}");
}

#[test]
fn untranslatable_loop_without_newly_admitted_opcodes_keeps_its_baseline() {
    // Regression: the settle must not demote Tier 1 code that already ran
    // natively at 82d3808. String literals (`push_atom_value`) have no Tier 2
    // classification, but this loop was a Tier 1 function before the
    // GENERIC_OP opcodes were admitted and must keep entering native code.
    let entries = native_entry_windows_after_install(
        "globalThis.f = function(n, z) { let sum = z; \
           for (let i = z; i < n; i++) sum = sum + i * 0.5; return 'r' + sum; };",
        "f(64, 0)",
        "r1008",
    );
    assert!(
        entries.windows(2).all(|pair| pair[1] > pair[0]),
        "Tier 1 function stopped entering native code: {entries:?}"
    );
}

#[test]
fn untranslatable_object_loops_stay_exact_after_their_bounded_trial() {
    // Object literals (`object`) and `new` (`call_constructor`) have no Tier 2
    // classification either. With complete feedback these loops take the
    // bounded Tier 2 trial, whose `UnsupportedOpcode` failures return them to
    // the interpreter; that is measured as faster than keeping every such
    // baseline (for example `calls-closures`). Every window must stay exact.
    let cases = [
        (
            "globalThis.f = function(n) { let sum = 0; \
               for (let i = 0; i < n; i++) { const o = { v: i }; sum = sum + o.v; } \
               return '' + sum; };",
            "f(64)",
            "2016",
        ),
        (
            "function P(v) { this.v = v; } \
             globalThis.f = function(n) { let sum = 0; \
               for (let i = 0; i < n; i++) { const p = new P(i); sum = sum + p.v; } \
               return '' + sum; };",
            "f(64)",
            "2016",
        ),
    ];
    for (source, call, expected) in cases {
        let entries = native_entry_windows_after_install(source, call, expected);
        assert!(entries[0] > 0, "{source} {entries:?}");
    }
}

#[test]
fn automatic_tiering_keeps_typeof_property_loops_interpreted() {
    // The P2b settle x P4c terminal-refresh interaction cannot arise while
    // automatic tiering defers `typeof`: the loop is never installed.
    interpreted_after_rounds(
        "globalThis.points = [{ x: 1, y: 'a' }, { x: 2, y: 3 }, { x: 3, y: 'b' }]; \
         globalThis.f = function(items) { let n = 0; \
           for (let i = 0; i < items.length; i++) { \
             const p = items[i]; if (typeof p.y === 'string') n = n + p.x; } \
           return '' + n; };",
        "f(points)",
        "4",
    );
}

/// Runs `call` for 4,000 rounds under automatic tiering and asserts it stays
/// exact and interpreted, rejected at Tier 1.
fn interpreted_after_rounds(source: &str, call: &str, expected: &str) -> rquickjs_jit::JitMetrics {
    let (_runtime, jit, context) = automatic_runtime();
    context.with(|ctx| ctx.eval::<(), _>(source).unwrap());
    for _ in 0..4_000 {
        let value = context.with(|ctx| ctx.eval::<String, _>(call).unwrap());
        assert_eq!(value, expected);
        jit.poll();
    }
    let metrics = jit.metrics();
    assert_eq!(metrics.native_entries, 0, "{metrics:?}");
    assert!(metrics.tier1_rejections > 0, "{metrics:?}");
    metrics
}

/// Runs `call` repeatedly under automatic tiering and returns the
/// `native_entries` metric sampled at the first native entry and after each
/// following 1,000-round window.
fn native_entry_windows_after_install(source: &str, call: &str, expected: &str) -> Vec<u64> {
    let (_runtime, jit, context) = automatic_runtime();
    context.with(|ctx| ctx.eval::<(), _>(source).unwrap());
    let run = || {
        let value = context.with(|ctx| ctx.eval::<String, _>(call).unwrap());
        assert_eq!(value, expected);
        jit.poll();
    };
    wait_for_native_entry(&jit, run);
    let mut entries = vec![jit.metrics().native_entries];
    for _ in 0..3 {
        for _ in 0..1_000 {
            run();
        }
        entries.push(jit.metrics().native_entries);
    }
    entries
}

/// Runs `run` until the first native entry. The baseline compiles in the
/// background, and unoptimized (debug, coverage) builds or a busy host may
/// install it only after many calls.
fn wait_for_native_entry(jit: &rquickjs_jit::Jit, run: impl Fn()) {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while jit.metrics().native_entries == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "never entered native code: {:?}",
            jit.metrics()
        );
        run();
    }
}
