#![cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

//! Tier 1 lowering of the synchronous iteration opcodes (`for_of_*`,
//! `for_in_*`, `iterator_close`). Every case compares the forced Tier 1
//! execution against a fresh interpreter, including iterator-close ordering on
//! `break` and on exceptions thrown by the loop body.

use rquickjs::{Context, Runtime};
use rquickjs_jit::bytecode::HelperId;
use rquickjs_jit::test_support::{differential, forced_baseline, SnapshotFixture};
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::time::{Duration, Instant};

const ITERATOR_OP: u32 = rquickjs::qjs::JSJitHelperId_JS_JIT_HELPER_ITERATOR_OP;
const SUM_OF: &str =
    "function f(values){let sum=0;for(const value of values)sum+=value;return sum}";

#[test]
fn packed_array_for_of_uses_the_array_values_leaf() {
    // Dense packed arrays never reach the generic helper for their elements;
    // only completion (one `next` past the end) takes ITERATOR_OP.
    // for_of_start, the completing for_of_next and iterator_close only.
    differential(SUM_OF, "f([1,2,3,4,5,6,7,8,9,10])")
        .force_baseline()
        .expect_executed_opcode("for_of_next")
        .expect_helper_call_count(ITERATOR_OP, 3)
        .assert_same();
    differential(
        SUM_OF,
        "f([0.5,'x',null,undefined,{},[1],1n,Symbol.iterator.description])",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("for_of_next")
    .expect_helper_call_count(ITERATOR_OP, 3)
    .assert_same();
    differential(SUM_OF, "f(['a','b',{toString(){return 'c'}}])")
        .force_baseline()
        .stress_gc()
        .expect_executed_opcode("for_of_start")
        .expect_helper(HelperId::IteratorOp)
        .assert_same();
    differential(SUM_OF, "f([])")
        .force_baseline()
        .expect_executed_opcode("iterator_close")
        .assert_same();
}

#[test]
fn non_packed_and_non_array_iterables_take_the_exact_helper() {
    for expression in [
        // Holes read through the prototype chain.
        "(()=>{Array.prototype[1]=40;try{return f([1,,3])}finally{delete Array.prototype[1]}})()",
        // `length` above the dense count keeps a fast array.
        "(()=>{const a=[1,2];a.length=5;return String(f(a))})()",
        "f(new Set([1,2,3]))",
        "f(new Map([[1,2]]).values())",
        "f('abc')",
        "f(new Int32Array([4,5,6]))",
        "f((function*(){yield 1;yield 2})())",
        "f({[Symbol.iterator](){let i=0;return {next(){return {value:i,done:i++>=3}}}}})",
        "(()=>{const a=[1,2,3];a.foo=1;return f(a.entries().next().value)})()",
    ] {
        differential(SUM_OF, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("for_of_next")
            .expect_helper(HelperId::IteratorOp)
            .assert_same();
    }
}

#[test]
fn array_mutation_during_iteration_matches_the_builtin_iterator() {
    let definition = "function f(values){let out=[];for(const value of values){out.push(value);if(value===2)values.push(9);if(value===9)values.length=0}return out.join(',')}";
    differential(definition, "f([1,2,3])")
        .force_baseline()
        .expect_executed_opcode("for_of_next")
        .assert_same();
    let shrink = "function f(values){let out=[];for(const value of values){out.push(value);values.pop()}return out.join(',')}";
    differential(shrink, "f([1,2,3,4,5])")
        .force_baseline()
        .expect_executed_opcode("for_of_next")
        .assert_same();
}

#[test]
fn replaced_array_iteration_protocol_is_observed() {
    // A user-installed %ArrayIteratorPrototype%.next is called for every step.
    differential(
        SUM_OF,
        "(()=>{const proto=Object.getPrototypeOf([][Symbol.iterator]());const next=proto.next;let calls=0;proto.next=function(){calls++;return next.call(this)};try{return f([1,2,3])+':'+calls}finally{proto.next=next}})()",
    )
    .force_baseline()
    .expect_executed_opcode("for_of_next")
    .expect_helper(HelperId::IteratorOp)
    .assert_same();
    // A replaced Array.prototype[Symbol.iterator] yields a user iterator.
    differential(
        SUM_OF,
        "(()=>{const original=Array.prototype[Symbol.iterator];Array.prototype[Symbol.iterator]=function*(){yield 40;yield 2};try{return f([1,2,3])}finally{Array.prototype[Symbol.iterator]=original}})()",
    )
    .force_baseline()
    .expect_executed_opcode("for_of_start")
    .expect_helper(HelperId::IteratorOp)
    .assert_same();
}

const LOGGING_ITERABLE: &str = "function iterable(log,limit,failNext){return {[Symbol.iterator](){log.push('iter');let i=0;return {next(){log.push('next'+i);if(failNext&&i===failNext)throw new Error('next');return {value:i,done:i++>=limit}},return(){log.push('return');return {}}}}}}";

#[test]
fn break_closes_the_iterator_exactly_once() {
    let definition = format!(
        "{LOGGING_ITERABLE}\nfunction f(it,stop){{let sum=0;for(const value of it){{if(value===stop)break;sum+=value}}return sum}}"
    );
    differential(
        &definition,
        "(()=>{const log=[];const r=f(iterable(log,5),2);return r+':'+log.join(',')})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("iterator_close")
    .expect_helper(HelperId::IteratorOp)
    .assert_same();
    // Completion does not call `return`.
    differential(
        &definition,
        "(()=>{const log=[];const r=f(iterable(log,3),99);return r+':'+log.join(',')})()",
    )
    .force_baseline()
    .expect_executed_opcode("iterator_close")
    .assert_same();
}

#[test]
fn iterator_close_errors_and_non_object_results_propagate() {
    let definition = "function f(it){for(const value of it){if(value===1)break}return 'done'}";
    differential(
        definition,
        "(()=>{try{return f({[Symbol.iterator](){let i=0;return {next(){return {value:i++,done:false}},return(){throw new RangeError('close')}}}})}catch(e){return e.name}})()",
    )
    .force_baseline()
    .expect_executed_opcode("iterator_close")
    .assert_same();
    differential(
        definition,
        "(()=>{try{return f({[Symbol.iterator](){let i=0;return {next(){return {value:i++,done:false}},return(){return 1}}}})}catch(e){return e.name}})()",
    )
    .force_baseline()
    .expect_executed_opcode("iterator_close")
    .assert_same();
}

#[test]
fn body_exceptions_close_the_iterator_with_a_throw_completion() {
    let definition = format!(
        "{LOGGING_ITERABLE}\nfunction f(it,g){{let sum=0;for(const value of it)sum+=g(value);return sum}}"
    );
    differential(
        &definition,
        "(()=>{const log=[];try{f(iterable(log,5),v=>{if(v===2)throw new TypeError('body');return v})}catch(e){log.push(e.name)}return log.join(',')})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("for_of_next")
    .assert_same();
    // The throw completion wins over an exception thrown by `return`.
    differential(
        &definition,
        "(()=>{const log=[];const it={[Symbol.iterator](){return {next(){return {value:1,done:false}},return(){log.push('return');throw new RangeError('close')}}}};try{f(it,()=>{throw new TypeError('body')})}catch(e){log.push(e.name)}return log.join(',')})()",
    )
    .force_baseline()
    .expect_executed_opcode("for_of_next")
    .assert_same();
    // Packed arrays: the leaf-produced value throws in the body.
    differential(
        &definition,
        "(()=>{try{return f([1,2,3],v=>{if(v===3)throw new Error('x');return v})}catch(e){return e.message}})()",
    )
    .force_baseline()
    .expect_executed_opcode("for_of_next")
    .assert_same();
}

#[test]
fn iterator_protocol_errors_do_not_close_the_iterator() {
    let definition = format!(
        "{LOGGING_ITERABLE}\nfunction f(it){{let sum=0;for(const value of it)sum+=value;return sum}}"
    );
    differential(
        &definition,
        "(()=>{const log=[];try{f(iterable(log,5,2))}catch(e){log.push(e.message)}return log.join(',')})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("for_of_next")
    .expect_helper(HelperId::IteratorOp)
    .assert_same();
    for expression in [
        "(()=>{try{return f(42)}catch(e){return e.name}})()",
        "(()=>{try{return f({[Symbol.iterator](){return 1}})}catch(e){return e.name}})()",
        "(()=>{try{return f({[Symbol.iterator](){return {next(){return 1}}}})}catch(e){return e.name}})()",
        "(()=>{try{return f({[Symbol.iterator](){return {get next(){throw new SyntaxError('n')}}}})}catch(e){return e.name}})()",
        "(()=>{try{return f({[Symbol.iterator](){return {next(){return {get done(){throw new URIError('d')}}}}}})}catch(e){return e.name}})()",
    ] {
        differential(&definition, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("for_of_start")
            .assert_same();
    }
}

#[test]
fn nested_for_of_loops_close_inner_iterators_on_outer_break() {
    let definition = format!(
        "{LOGGING_ITERABLE}\nfunction f(log){{let out=0;outer:for(const a of iterable(log,3)){{for(const b of iterable(log,3)){{if(a===1&&b===1)break outer;out+=a*10+b}}}}return out}}"
    );
    differential(
        &definition,
        "(()=>{const log=[];return f(log)+':'+log.join(',')})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("iterator_close")
    .assert_same();
}

#[test]
fn for_in_enumerates_like_the_interpreter() {
    let definition =
        "function f(o,g){let keys=[];for(const key in o){keys.push(key);if(key==='b')g(o)}return keys.join(',')}";
    for expression in [
        "f({a:1,b:2,c:3,d:4},o=>{delete o.c})",
        "f({a:1,b:2,c:3},o=>{o.z=1})",
        "f([5,6,7],()=>{})",
        "f(Object.create({inherited:1},{own:{value:2,enumerable:true}}),()=>{})",
        "f(null,()=>{})",
        "f(undefined,()=>{})",
        "f('xy',()=>{})",
        "f(new Proxy({p:1,q:2},{}),()=>{})",
    ] {
        differential(definition, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("for_in_next")
            .expect_helper(HelperId::IteratorOp)
            .assert_same();
    }
    differential(
        definition,
        "(()=>{try{return f(new Proxy({},{ownKeys(){throw new EvalError('keys')}}),()=>{})}catch(e){return e.name}})()",
    )
    .force_baseline()
    .expect_executed_opcode("for_in_start")
    .assert_same();
}

#[test]
fn array_destructuring_and_rest_use_offset_for_of_next() {
    // A `...rest` element keeps an Int32 index on the operand stack across
    // its loop, which the verifier's exact operand-stack merge rejects; the
    // offset form of `for_of_next` is covered by the nested elements here.
    let definition = "function f(values){const [a,,b='seven']=values;const [[c,d]=[5,6]]=[];return a+':'+b+':'+c+d}";
    for expression in [
        "f([1,2,3,4,5])",
        "f([1])",
        "f('hello')",
        "f(new Set([9,8]))",
    ] {
        differential(definition, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("for_of_next")
            .assert_same();
    }
    let close = "function f(it){const [a,b]=it;return a+b}";
    differential(
        &format!("{LOGGING_ITERABLE}\n{close}"),
        "(()=>{const log=[];return f(iterable(log,9))+':'+log.join(',')})()",
    )
    .force_baseline()
    .expect_executed_opcode("iterator_close")
    .assert_same();
}

#[test]
fn return_inside_for_of_closes_the_iterator_in_tier1() {
    // `return` from inside a for-of emits `nip_catch` over the for-of catch
    // offset, then `iterator_close`. P2c lowers the direct `nip_catch`; the
    // for-of close offset is not a try region, so the function stays native
    // and `iterator_close` runs the iterator's `return` exactly once.
    let definition =
        "function f(values){for(const value of values)if(value>1)return value;return 0}";
    for expression in [
        "f([1,2,3])",
        "f([0,1])",
        "(()=>{const events=[];const it={[Symbol.iterator](){let i=0;return {next(){events.push('next'+i);return {value:i,done:i++>=5}},return(){events.push('return');return {}}}}};return [f(it),events.join()]})()",
        "(()=>{const it={[Symbol.iterator](){let i=0;return {next(){return {value:i,done:i++>=5}},return(){throw new RangeError('close')}}}};try{return f(it)}catch(e){return e.name}})()",
        "(()=>{const it={[Symbol.iterator](){let i=0;return {next(){return {value:i,done:i++>=5}},return(){return 1}}}};try{return f(it)}catch(e){return e.name}})()",
    ] {
        differential(definition, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("nip_catch")
            .expect_executed_opcode("iterator_close")
            .expect_helper(HelperId::IteratorOp)
            .assert_same();
    }
}

#[test]
fn for_of_close_offsets_alone_are_not_exception_regions() {
    // Tier 2, inlining, tagged links and the automatic-tier profitability
    // demotion skip functions with exception regions. A for-of close offset
    // (catch offset 0) is unwound by the interpreter, so it must not make a
    // plain for-of loop look like a try region, and it must stay live on the
    // loop backedge for the verifier's catch-offset merge.
    for (source, regions) in [
        (
            "(function f(values){let sum=0;for(const value of values)sum+=value;return sum})",
            false,
        ),
        (
            "(function f(values){let n=0;for(const key in values)n++;return n})",
            false,
        ),
        (
            "(function f(values){try{for(const value of values)if(value)throw value}catch(e){return e}return 0})",
            true,
        ),
        (
            "(function f(values){for(const value of values)if(value>1)return value;return 0})",
            true,
        ),
    ] {
        let fixture = SnapshotFixture::compile(source);
        let verified = fixture
            .snapshot()
            .verify(Default::default())
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        assert_eq!(verified.has_exception_regions(), regions, "{source}");
    }
}

#[test]
fn try_catch_inside_a_for_of_body_resumes_its_handler_natively() {
    // The innermost catch offset is the try's, above the live for-of close
    // offset: the P2c landing pad resumes the handler and the loop continues.
    let definition = "function f(values){const out=[];for(const value of values){try{if(value===2)throw value;out.push(value)}catch(e){out.push('c'+e)}}return out.join()}";
    for expression in [
        "f([1,2,3])",
        "f(new Set([2,2,4]))",
        "(()=>{const events=[];const it={[Symbol.iterator](){let i=0;return {next(){return {value:i,done:i++>=4}},return(){events.push('return');return {}}}}};return [f(it),events.join()]})()",
    ] {
        differential(definition, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("catch")
            .expect_executed_opcode("for_of_next")
            .assert_same();
    }
}

#[test]
fn for_of_inside_a_try_region_closes_the_iterator_before_the_handler() {
    // An exception under a for-of close offset nested in a try region must
    // close the iterator (throw completion) before the handler runs. A P2c
    // landing pad would skip that close, so such exceptions take the ordinary
    // native exception exit and the interpreter unwinds the published frame:
    // it closes the iterator, then resumes the handler.
    let definition = "globalThis.kernel=function kernel(values,events){try{for(const value of values){if(value===3)throw new TypeError('t'+value);events.push(value)}}catch(e){events.push('caught '+e.message)}return events.join()}";
    let expression = "(()=>{const events=[];const it={[Symbol.iterator](){let i=0;return {next(){return {value:i,done:i++>=6}},return(){events.push('return');return {}}}}};return kernel(it,events)+'|'+kernel([1,2,3,4],[])})()";
    let interpreter = Runtime::new().unwrap();
    let interpreter_context = Context::full(&interpreter).unwrap();
    let expected: String = interpreter_context.with(|ctx| {
        ctx.eval::<(), _>(definition).unwrap();
        ctx.eval(expression).unwrap()
    });
    assert_eq!(expected, "0,1,2,return,caught t3|1,2,caught t3");
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
    context.with(|ctx| ctx.eval::<(), _>(definition).unwrap());
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    for iteration in 0.. {
        let result: String = context.with(|ctx| ctx.eval(expression).unwrap());
        assert_eq!(result, expected);
        jit.poll();
        if (iteration >= 64 && jit.metrics().native_entries >= 8) || Instant::now() > deadline {
            break;
        }
    }
    let metrics = jit.metrics();
    assert!(metrics.native_entries >= 8, "{metrics:?}");
    assert!(metrics.native_exits >= 8, "{metrics:?}");
}

#[test]
fn production_tiering_enters_native_code_for_a_for_of_kernel() {
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
            "globalThis.kernel=function kernel(n){const values=[];let sum=0;for(let i=0;i<n;i++)values.push((i*17)&1023);for(const value of values)sum+=value;return String(sum)}",
        )
        .unwrap()
    });
    let expected: String = context.with(|ctx| {
        ctx.eval("(function(n){let s=0;for(let i=0;i<n;i++)s+=(i*17)&1023;return String(s)})(500)")
            .unwrap()
    });
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 50 } else { 10 });
    let mut result = String::new();
    while Instant::now() < deadline {
        result = context.with(|ctx| ctx.eval("kernel(500)").unwrap());
        assert_eq!(result, expected);
        jit.poll();
        if jit.metrics().native_entries > 0 {
            break;
        }
    }
    assert_eq!(result, expected);
    assert!(jit.metrics().native_entries > 0, "{:?}", jit.metrics());
}

#[test]
fn leaf_path_storage_transitions_and_array_likes_match_the_interpreter() {
    // Fast-to-slow storage transitions inside the body must leave the leaf
    // (the per-step fast_array/count revalidation) without skipping or
    // repeating an element.
    let log = "function f(values,mutate){let out=[];for(const value of values){out.push(String(value));mutate(values,out.length)}return out.join(',')}";
    for expression in [
        "f([1,2,3,4],(a,n)=>{if(n===2)a[10]=7})",
        "f([1,2,3,4],(a,n)=>{if(n===1)delete a[2]})",
        "f([1,2,3,4],(a,n)=>{if(n===2)Object.defineProperty(a,3,{get(){return 'g'}})})",
        "f([1,2,3,4],(a,n)=>{if(n===2)Object.freeze(a)})",
        "f([1,2,3,4],(a,n)=>{if(n===1)a.length=2})",
    ] {
        differential(log, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("for_of_next")
            .assert_same();
    }
    // Array-likes and subclasses: each either hits the Array values leaf or
    // takes the exact helper, and matches the interpreter.
    for expression in [
        "(function(){return f(arguments)})(1,2,3)",
        "(()=>{class A extends Array{};const a=new A();a.push(4,5,6);return f(a)})()",
        "(()=>{class A extends Array{*[Symbol.iterator](){yield 40;yield 2}};return f(A.from([1,2,3]))})()",
        "f({[Symbol.iterator](){return Array.prototype.values.call({length:3,0:1,1:2,2:3})}})",
        "f({[Symbol.iterator](){return Array.prototype.values.call([7,8,9])}})",
        "f({[Symbol.iterator](){return Array.prototype.keys.call([7,8,9])}})",
    ] {
        differential(SUM_OF, expression)
            .force_baseline()
            .stress_gc()
            .expect_executed_opcode("for_of_next")
            .assert_same();
    }
}

#[test]
fn interrupt_inside_a_leaf_for_of_loop_is_uncatchable() {
    forced_baseline(
        "function f(values){let sum=0;for(const value of values)sum+=value;return sum} f(new Array(200000).fill(1))",
    )
    .interrupt_after(3)
    .assert_uncatchable_interrupt();
}

fn osr_first_invocation(source: &str, expected: f64) {
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
    let value = context
        .with(|ctx| ctx.eval::<f64, _>(source))
        .unwrap_or_else(|error| panic!("{error:?}; {:?}", jit.metrics()));
    assert_eq!(value, expected, "{:?}", jit.metrics());
    assert!(jit.metrics().osr_entries >= 1, "{:?}", jit.metrics());
}

#[test]
fn long_first_for_of_invocation_osr_enters_with_the_catch_offset_on_the_stack() {
    osr_first_invocation(
        "const values=new Array(4000000).fill(3);function f(values){let sum=0;for(const value of values)sum+=value;return sum} f(values)",
        12_000_000.0,
    );
}

#[test]
fn long_first_for_in_invocation_osr_enters_with_the_enumerator_on_the_stack() {
    osr_first_invocation(
        "const object={};for(let i=0;i<300000;i++)object['k'+i]=1;function f(object){let sum=0;for(const key in object)sum+=object[key];return sum} f(object)",
        300_000.0,
    );
}
