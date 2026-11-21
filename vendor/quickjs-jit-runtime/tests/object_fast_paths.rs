#![cfg(all(
    feature = "compiler",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
//! Tier 1 object-literal and `Array.prototype.push` fast paths (ABI 1.25
//! `JSJitObjectAPI`). Every scenario is checked against the interpreter; the
//! helper counters prove that hits skip the generic helpers while misses keep
//! the exact helper semantics.

use rquickjs::{Context, Function, Runtime};
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn JS_JitGetHelperCount(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        helper_id: u32,
        count: *mut u64,
    ) -> i32;
}

fn helper_count(rt: *mut rquickjs_core::qjs::JSRuntime, helper_id: u32) -> u64 {
    let mut count = 0;
    assert_eq!(
        unsafe { JS_JitGetHelperCount(rt, helper_id, &mut count) },
        0
    );
    count
}

fn interpreted(source: &str, expression: &str) -> String {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(source).unwrap();
        ctx.eval::<String, _>(expression).unwrap()
    })
}

struct Native {
    _jit: Jit,
    context: Context,
    rt: *mut rquickjs_core::qjs::JSRuntime,
}

impl Native {
    fn start(source: &str, expression: &str, stress_gc: bool) -> (Self, String) {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .tier_policy(JitTierPolicy::BaselineOnly)
                .call_threshold(1)
                .loop_threshold(1)
                .stress_gc(stress_gc)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
        context
            .with(|ctx| {
                ctx.eval::<(), _>(format!(
                    "globalThis.__objectFastInvoke=function(){{return {expression}}}"
                ))
            })
            .unwrap();
        let native = Self {
            _jit: jit,
            rt: context
                .with(|ctx| unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) }),
            context,
        };
        // Sanitizer builds compile far more slowly (CI runners took over 30 s
        // for one of these compiles).
        let budget = if cfg!(rquickjs_sanitizer) { 300 } else { 60 };
        let deadline = Instant::now() + Duration::from_secs(budget);
        let mut result = native.invoke();
        loop {
            native._jit.poll();
            let metrics = native._jit.metrics();
            if metrics.native_entries > 0 && metrics.pending_worker_jobs == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "{metrics:?}");
            std::thread::sleep(Duration::from_millis(1));
            result = native.invoke();
        }
        (native, result)
    }

    fn invoke(&self) -> String {
        self.context.with(|ctx| {
            let invoke: Function<'_> = ctx.globals().get("__objectFastInvoke").unwrap();
            let result = invoke.call::<_, String>(());
            assert!(result.is_ok(), "{result:?}: {:?}", ctx.catch());
            result.unwrap()
        })
    }

    fn helper(&self, id: u32) -> u64 {
        helper_count(self.rt, id)
    }
}

fn assert_differential(source: &str, expression: &str, stress_gc: bool) -> Native {
    let expected = interpreted(source, expression);
    let (native, first) = Native::start(source, expression, stress_gc);
    assert_eq!(first, expected, "warmup result");
    for _ in 0..8 {
        assert_eq!(native.invoke(), expected);
        native._jit.poll();
    }
    assert!(native._jit.metrics().native_entries > 0);
    native
}

const LITERALS: &str = r#"
function make(i, s, o) {
    const plain = {a: i, b: s, c: true, d: null, e: undefined, f: -1, g: o};
    const numeric = {0: i, 1: s};
    const dup = {k: i, k: s};
    const empty = {};
    return [plain, numeric, dup, empty];
}
function run() {
    const keep = [];
    let text = '';
    for (let i = 0; i < 300; i++) {
        const shared = {tag: i};
        const made = make(i, 'v' + (i & 3), shared);
        keep.push(made);
        text = Object.keys(made[0]).join(',') + '|' + JSON.stringify(made[0]) + '|' +
            JSON.stringify(made[1]) + '|' + JSON.stringify(made[2]) + '|' +
            Object.keys(made[3]).length;
    }
    const first = keep[3][0];
    first.z = 1;
    delete first.a;
    return text + '|' + keep.length + '|' + JSON.stringify(first) + '|' +
        JSON.stringify(keep[4][0]) + '|' + (keep[5][0].g === keep[5][0].g) + '|' +
        Object.isExtensible(keep[6][0]) + '|' +
        JSON.stringify(Object.getOwnPropertyDescriptor(keep[7][0], 'a'));
}
"#;

#[test]
fn object_literals_match_the_interpreter() {
    assert_differential(LITERALS, "run()", false);
}

#[test]
fn complete_object_literals_skip_allocation_and_define_helpers() {
    let source = r#"
function make(i, s, o) {
    const point = {x: i, y: s, owner: o, flag: false};
    const pair = {0: i, 1: s};
    return [point, pair];
}
function run() {
    const keep = [];
    for (let i = 0; i < 200; i++) keep.push(make(i, 'v' + (i & 7), keep));
    return JSON.stringify(keep[123][1]) + ':' + keep[199][0].x + keep[5][0].y +
        (keep[9][0].owner === keep) + keep[3][0].flag;
}
"#;
    let native = assert_differential(source, "run()", false);
    let new_object = native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_NEW_OBJECT);
    let set_property = native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_PROPERTY);
    native.invoke();
    // Once each final shape exists, `make` builds both literals in the leaf.
    assert_eq!(
        native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_NEW_OBJECT) - new_object,
        0
    );
    // Only the two `array_from` element stores of `[point, pair]` remain.
    assert_eq!(
        native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_SET_PROPERTY) - set_property,
        400
    );
}

#[test]
fn object_literals_survive_stress_gc() {
    assert_differential(LITERALS, "run()", true);
}

#[test]
fn object_literal_defines_own_fields_despite_prototype_accessors() {
    let source = r#"
let hits = 0;
Object.defineProperty(Object.prototype, 'x', {
    set(v) { hits++; }, get() { return 'proto'; }, configurable: true
});
function lit(i) { return {x: i, y: 2}; }
function run() {
    let text = '';
    for (let i = 0; i < 200; i++) {
        const o = lit(i);
        text = o.x + ':' + Object.getOwnPropertyNames(o).join(',') + ':' + o.hasOwnProperty('x');
    }
    return text + ':' + hits;
}
"#;
    assert_differential(source, "run()", false);
}

#[test]
fn object_literal_uses_the_current_object_prototype() {
    let source = r#"
function lit(i) { return {p: i}; }
function run() {
    let text = '';
    for (let i = 0; i < 100; i++) text = String(lit(i).marker);
    Object.prototype.marker = 'installed';
    for (let i = 0; i < 100; i++) text += ':' + lit(i).marker;
    delete Object.prototype.marker;
    return text + ':' + (Object.getPrototypeOf(lit(1)) === Object.prototype);
}
"#;
    assert_differential(source, "run()", false);
}

const PUSHES: &str = r#"
function pushAll(array, count, base) {
    let length = 0;
    for (let i = 0; i < count; i++) length = array.push(base + i);
    return length;
}
function pushObjects(array, count) {
    for (let i = 0; i < count; i++) array.push({index: i, score: i * 3});
    return array.length;
}
function attempt(array, count) {
    try { return String(pushAll(array, count, 0)); } catch (e) { return e.constructor.name; }
}
function run() {
    const out = [];
    const grow = [];
    out.push(pushAll(grow, 1000, 1), grow[0], grow[999], grow.length);
    const objects = [];
    out.push(pushObjects(objects, 100), objects[50].score);
    out.push(attempt(Object.freeze([1]), 1));
    const sealed = Object.seal([1, 2]);
    out.push(attempt(sealed, 1), sealed.length);
    const readonly = [1, 2];
    Object.defineProperty(readonly, 'length', {writable: false});
    out.push(attempt(readonly, 1), readonly.length);
    const own = [];
    own.push = function (v) { return 'own:' + v; };
    out.push(pushAll(own, 2, 5), own.length);
    class Sub extends Array { push(v) { return super.push(v * 10); } }
    const sub = new Sub();
    out.push(pushAll(sub, 3, 1), JSON.stringify(sub));
    const sparse = [1, 2, 3];
    sparse.length = 6;
    out.push(pushAll(sparse, 2, 7), JSON.stringify(sparse));
    const holey = [1, , 3];
    out.push(pushAll(holey, 1, 9), JSON.stringify(holey));
    const arrayLike = {length: 1, push: Array.prototype.push};
    out.push(pushAll(arrayLike, 2, 4), JSON.stringify(arrayLike));
    const saved = Array.prototype.push;
    let intercepted = 0;
    Array.prototype.push = function (v) { intercepted++; return saved.call(this, -v); };
    const patched = [];
    out.push(pushAll(patched, 3, 1), JSON.stringify(patched), intercepted);
    Array.prototype.push = saved;
    const unshift = [];
    unshift.push = Array.prototype.unshift;
    out.push(pushAll(unshift, 3, 1), JSON.stringify(unshift));
    Object.defineProperty(Array.prototype, 'push', {
        get() { intercepted += 100; return saved; }, configurable: true
    });
    const accessor = [];
    out.push(pushAll(accessor, 2, 1), intercepted);
    Object.defineProperty(Array.prototype, 'push', {
        value: saved, writable: true, enumerable: false, configurable: true
    });
    return JSON.stringify(out);
}
"#;

#[test]
fn array_push_matches_the_interpreter_on_hits_and_every_miss_shape() {
    assert_differential(PUSHES, "run()", false);
}

#[test]
fn array_push_survives_stress_gc() {
    assert_differential(PUSHES, "run()", true);
}

#[test]
fn packed_array_push_skips_the_lookup_and_call_helpers() {
    let source = r#"
function fill(count) {
    const values = [];
    for (let i = 0; i < count; i++) values.push((i * 17) & 1023);
    let sum = 0;
    for (let i = 0; i < values.length; i++) sum += values[i];
    return sum;
}
"#;
    let native = assert_differential(source, "String(fill(500))", false);
    let call = native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
    let get = native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY);
    native.invoke();
    // Only the native invocation wrapper's `fill(...)` and `String(...)` calls
    // use the generic CALL helper; the 500 pushes and their `push` lookups
    // are handled by the leaves.
    assert_eq!(
        native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL) - call,
        2
    );
    let lookups = native.helper(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_GET_PROPERTY) - get;
    // `values.length` (501 reads) still uses the generic lookup helper.
    assert!(lookups <= 501, "{lookups}");
}

#[test]
fn polymorphic_object_workload_matches_the_interpreter() {
    let source = r#"
function workload(iterations, seed) {
  const objects = [];
  for (let i = 0; i < iterations; i++) {
    let object;
    switch (i & 3) {
      case 0: object = { x: i, y: seed, kind: 0 }; break;
      case 1: object = { y: seed, x: i, extra: 1, kind: 1 }; break;
      case 2: object = { x: i, kind: 2 }; object.y = seed; break;
      default: object = Object.create(null); object.kind = 3; object.x = i; object.y = seed;
    }
    object.x += object.kind;
    objects.push(object);
  }
  let sum = 0;
  for (let i = 0; i < objects.length; i++) sum += objects[i].x + objects[i].y;
  return sum;
}
"#;
    assert_differential(source, "String(workload(2000, 7))", true);
}

#[test]
fn push_statements_on_non_arrays_and_uninitialized_bindings_stay_exact() {
    let source = r#"
function drive(target, value) {
    let out = 0;
    for (let i = 0; i < 50; i++) out = target.push(value);
    return out;
}
function lexical(flag) {
    if (flag) return early.push(1);
    let early = [];
    early.push(2);
    return early.length;
}
function run() {
    const out = [];
    const counter = {n: 0, push(v) { this.n += v; return this.n; }};
    out.push(drive(counter, 2), drive([], 'x'), drive(new Array(3), 1));
    try { lexical(true); } catch (e) { out.push(e.constructor.name); }
    out.push(lexical(false));
    try { drive(null, 1); } catch (e) { out.push(e.constructor.name); }
    return JSON.stringify(out);
}
"#;
    assert_differential(source, "run()", true);
}

/// Pre-existing on 82d3808: the generic Tier 1 lowering implements
/// `define_field` with set semantics (`JS_JIT_HELPER_SET_PROPERTY`), so an
/// inherited setter runs and no own property is created. Only literals that
/// the new leaf builds are exact; this non-fusable literal still diverges.
/// Fixing it needs an exact define helper (see docs/roadmap/p4e-alloc-fastpath.md).
#[test]
#[ignore = "pre-existing Tier 1 define_field set-semantics bug"]
fn generic_define_field_ignores_inherited_setters() {
    let source = r#"
let hits = 0;
Object.defineProperty(Object.prototype, 'x', {
    set(v) { hits++; }, get() { return 'proto'; }, configurable: true
});
function id(v) { return v; }
function lit(i) { return {x: i, y: id(2)}; }
function run() {
    let text = '';
    for (let i = 0; i < 200; i++) {
        const o = lit(i);
        text = o.x + ':' + Object.getOwnPropertyNames(o).join(',');
    }
    return text + ':' + hits;
}
"#;
    assert_differential(source, "run()", false);
}

/// Integration of the object/push leaves with Tier 1 try regions (P2c),
/// closure-captured locals (P2a) and for-of iteration (P2d). The fused
/// literal and push regions sit inside a try region whose fallback throws
/// (frozen receiver) and resumes the in-frame handler; the fused operands are
/// locals that a closure call rewrites between executions, so the leaves must
/// read the reloaded native copies; and the iteration value is a for-of
/// binding.
const INTERACTIONS: &str = r#"
function guarded(target, count) {
    let caught = 0;
    let last = null;
    for (let i = 0; i < count; i++) {
        try {
            last = {at: i, size: target.length};
            target.push(i);
        } catch (e) {
            caught++;
            last = {error: e.constructor.name, at: i};
        }
    }
    return caught + ':' + JSON.stringify(last) + ':' + target.length;
}
function captured(count) {
    let v = 0;
    const bump = () => { v += 3; };
    const out = [];
    for (let i = 0; i < count; i++) {
        bump();
        const o = {v: v, i: i};
        out.push(v);
        out.push(o);
    }
    return out.length + ':' + out[out.length - 2] + ':' + JSON.stringify(out[out.length - 1]);
}
function iterate(values) {
    const out = [];
    for (const value of values) {
        const o = {value: value};
        out.push(value);
        out.push(o);
    }
    return out.length + ':' + out[4] + ':' + JSON.stringify(out[5]);
}
function run() {
    const parts = [];
    for (let round = 0; round < 20; round++) {
        parts.length = 0;
        parts.push(guarded([], 40), guarded(Object.freeze([1, 2]), 40));
        const sealed = Object.seal([7]);
        parts.push(guarded(sealed, 5));
        parts.push(captured(50), iterate([4, 5, 6, 7, 8]));
    }
    return JSON.stringify(parts);
}
"#;

#[test]
fn object_fast_paths_compose_with_try_regions_closures_and_for_of() {
    assert_differential(INTERACTIONS, "run()", false);
}

#[test]
fn object_fast_paths_compose_with_try_regions_under_stress_gc() {
    assert_differential(INTERACTIONS, "run()", true);
}

#[test]
fn fused_leaves_hit_inside_try_regions_and_after_closure_writes() {
    use rquickjs_core::qjs::{
        JSJitHelperId_JS_JIT_HELPER_CALL as CALL,
        JSJitHelperId_JS_JIT_HELPER_NEW_OBJECT as NEW_OBJECT,
    };
    // P2a: `bump()` writes the captured `v` through its var ref; the literal
    // and push leaves then read the reloaded local without NEW_OBJECT or a
    // generic push CALL (only the 50 `bump()` calls and the wrapper's calls).
    let native = assert_differential(INTERACTIONS, "String(captured(50))", false);
    let (new_object, call) = (native.helper(NEW_OBJECT), native.helper(CALL));
    native.invoke();
    assert_eq!(native.helper(NEW_OBJECT) - new_object, 0);
    assert!(
        native.helper(CALL) - call <= 53,
        "{}",
        native.helper(CALL) - call
    );

    // P2c: the fused push inside the try region hits on a writable array ...
    let native = assert_differential(INTERACTIONS, "String(guarded([], 40))", false);
    let call = native.helper(CALL);
    native.invoke();
    assert!(
        native.helper(CALL) - call <= 3,
        "{}",
        native.helper(CALL) - call
    );

    // ... and on a frozen array misses into the generic CALL, which throws
    // into the in-frame handler 40 times without leaving native code.
    let native = assert_differential(
        INTERACTIONS,
        "String(guarded(Object.freeze([1]), 40))",
        false,
    );
    let entries = native._jit.metrics().native_entries;
    let call = native.helper(CALL);
    native.invoke();
    assert!(native.helper(CALL) - call >= 40);
    assert!(native._jit.metrics().native_entries - entries <= 2);
}
