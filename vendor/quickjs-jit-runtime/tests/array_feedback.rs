use std::sync::{Arc, Mutex};

use rquickjs::{Context, Runtime};
use rquickjs_core::{
    qjs,
    runtime::{JitBackend, RuntimeJitGuard},
};
use rquickjs_jit::runtime::{ArrayHazards, ArrayMode, FeedbackTable, FunctionKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArrayEvent {
    function: (u64, u64),
    pc: u32,
    mode: u32,
    flags: u32,
}

struct Capture(Arc<Mutex<Vec<ArrayEvent>>>);

unsafe impl JitBackend for Capture {
    fn record_feedback(&mut self, event: &qjs::JSJitFeedbackEvent) {
        if event.kind == qjs::JSJitFeedbackKind_JS_JIT_FEEDBACK_ARRAY {
            assert_eq!(event.type_count, 0);
            self.0.lock().unwrap().push(ArrayEvent {
                function: (event.function.id, event.function.generation),
                pc: event.pc,
                mode: event.slot,
                flags: event.flags,
            });
        }
    }
}

fn capture(source: &str) -> (String, Vec<ArrayEvent>) {
    let runtime = Runtime::new().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let _guard = RuntimeJitGuard::attach(&runtime, Capture(Arc::clone(&events))).unwrap();
    let context = Context::full(&runtime).unwrap();
    let value = context.with(|ctx| ctx.eval::<String, _>(source)).unwrap();
    let events = events.lock().unwrap().clone();
    (value, events)
}

#[test]
fn interpreter_profiles_each_actual_array_class_at_the_same_load_site() {
    let (value, events) = capture("function read(a){return a[0]} read([11]);read(new Int32Array([22]));read(new Float64Array([33]));'ok'");
    assert_eq!(value, "ok");
    assert_eq!(
        events.len(),
        3,
        "actual interpreter element observations: {events:?}"
    );
    assert_eq!(
        events.iter().map(|event| event.mode).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(events
        .iter()
        .all(|event| event.function == events[0].function
            && event.pc == events[0].pc
            && event.flags == 0));
}

#[test]
fn profiler_runs_before_proxy_traps_and_observes_fast_store_and_length() {
    let (value, events) = capture("function read(a){return a[0]} function write(a){a[0]=7;return a.length} const p=new Proxy([],{get(){return [].length}});read(p);String(write([0]))");
    assert_eq!(value, "1");
    assert_eq!(
        events
            .iter()
            .map(|event| (event.mode, event.flags))
            .collect::<Vec<_>>(),
        [(0, 1 << 8), (1, 1 << 7), (1, 1 << 6), (1, 1 << 7)]
    );
}

#[test]
fn interpreter_marks_resizable_fixed_views_detached_shared_immutable_and_slow_arrays() {
    let (value, events) = capture(
        r#"
        function read(a){return a[0]}
        const rab = new ArrayBuffer(16, {maxByteLength:32});
        read(new Int32Array(rab));
        read(new Int32Array(rab,0,2));
        const detached = new ArrayBuffer(8);
        const view = new Int32Array(detached);
        detached.transfer();
        read(view);
        read(new Float64Array(new SharedArrayBuffer(8)));
        read(new Int32Array(new ArrayBuffer(8).transferToImmutable()));
        const slow = [1,2];
        Object.defineProperty(slow, '0', {get(){return 3}});
        read(slow);
        'ok'
    "#,
    );
    assert_eq!(value, "ok");
    assert_eq!(
        events
            .iter()
            .map(|event| (event.mode, event.flags))
            .collect::<Vec<_>>(),
        [
            (2, 1 << 10),
            (2, 1 << 10),
            (2, 1 << 11),
            (3, 1 << 13),
            (2, 1 << 12),
            (0, 1 << 9)
        ]
    );
}

#[test]
fn actual_wire_observations_invalidate_new_snapshots_without_mutating_old_ones() {
    let (_, events) = capture("function read(a){return a[0]} read(new Int32Array(2));read(new Int32Array(new ArrayBuffer(8,{maxByteLength:16}),0,2));'ok'");
    assert_eq!(events.len(), 2);
    let key = FunctionKey::new(events[0].function.0, events[0].function.1);
    let mut table = FeedbackTable::new(1, 3);
    let observe = |table: &mut FeedbackTable, event: ArrayEvent| {
        table
            .observe_array_raw(
                FunctionKey::new(event.function.0, event.function.1),
                event.pc,
                event.mode,
                event.flags,
            )
            .unwrap();
    };
    observe(&mut table, events[0]);
    let before = table.snapshot(1);
    let site = before.array_at(key, events[0].pc).unwrap();
    assert_eq!(site.modes().collect::<Vec<_>>(), [ArrayMode::Int32]);
    assert!(site.can_specialize());
    observe(&mut table, events[1]);
    let after = table.snapshot(2);
    let site = after.array_at(key, events[0].pc).unwrap();
    assert_eq!(site.hazards(), ArrayHazards::RESIZABLE);
    assert!(!site.can_specialize());
    assert!(before.array_at(key, events[0].pc).unwrap().can_specialize());
    assert_eq!(table.version(), 2);
    observe(&mut table, events[1]);
    assert_eq!(table.version(), 2);
}
