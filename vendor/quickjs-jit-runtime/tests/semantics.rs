#![cfg(all(
    feature = "test-support",
    feature = "compiler",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]

use rquickjs_jit::bytecode::HelperId;
use rquickjs_jit::test_support::{differential, forced_baseline};

#[test]
fn compiled_values_have_interpreter_ownership() {
    differential("function f(o) { let x=o; return [x,x] }", "f({a:1})").assert_same();
}

#[test]
fn compiled_constant_outlives_the_worker_snapshot_and_registry_detaches() {
    // The forced backend compiles from a pointer-free snapshot, drops it
    // before this call, and retains the active bytecode only through the
    // RawRuntime-owned function registry. DifferentialRun also verifies that
    // detaching the guard makes the weak registry handle report detached.
    differential(
        "function f() { return 123456789012345678901234567890n }",
        "String(f())",
    )
    .force_baseline()
    .assert_same();
}

#[test]
fn thrown_getter_is_caught_at_the_same_handler() {
    differential(
        "function f(o){ return o.x }",
        "(() => { try { return f({get x(){throw new Error('boom')}}) } catch(e) { return e.message } })()",
    )
    .force_baseline()
    .assert_same();
}

#[test]
fn symbol_to_primitive_addition_preserves_event_order() {
    differential(
        r#"
        const events=[];
        function f(left, right){ return left + right }
        "#,
        r#"({
            value: f(
                { [Symbol.toPrimitive]() { events.push('left'); return 20 } },
                { [Symbol.toPrimitive]() { events.push('right'); return 22 } }
            ),
            events
        })"#,
    )
    .force_baseline()
    .assert_same();
}

#[test]
fn thrown_symbol_to_primitive_preserves_left_to_right_event_order() {
    differential(
        r#"
        const events=[];
        function f(left, right){ return left + right }
        "#,
        r#"(() => {
            try {
                f(
                    { [Symbol.toPrimitive]() { events.push('left'); return 20 } },
                    { [Symbol.toPrimitive]() { events.push('right'); throw new Error('boom') } }
                );
            } catch (error) {
                return { message: error.message, events };
            }
        })()"#,
    )
    .force_baseline()
    .assert_same();
}

#[test]
fn unary_plus_rejects_bigint_in_native_code() {
    differential(
        "function f(value){ return +value }",
        "(() => { try { return f(1n) } catch (error) { return `${error.name}:${error.message}` } })()",
    )
    .force_baseline()
    .assert_same();
}

#[test]
fn thrown_reentrant_call_clears_scratch_before_exception_transfer() {
    differential(
        "function f(callback, value){ callback(value); return 1 }",
        "(() => { try { f(() => { throw new Error('call boom') }, {marker: 1}) } catch (error) { return error.message } })()",
    )
    .force_baseline()
    .assert_same();
}

#[test]
fn interrupt_stops_compiled_loop() {
    forced_baseline("function f(){for(;;){}} f()")
        .interrupt_after(100)
        .assert_uncatchable_interrupt();
}

#[test]
fn primitive_numeric_loop_avoids_ownership_helpers() {
    differential(
        "function f(n) { let sum = 0; for (let i = 0; i < n; i++) sum = sum + i; return sum; }",
        "f(100)",
    )
    .force_baseline()
    .expect_ownership_helper_counts(0, 0)
    .assert_same();
}

#[test]
fn iterative_fibonacci_avoids_ownership_helpers() {
    differential(
        "function f(batches) { let result = 0; for (let batch = 0; batch < batches; batch++) { let a = 0; let b = 1; for (let i = 0; i < 40; i++) { const next = a + b; a = b; b = next; } result = a; } return result; }",
        "f(1)",
    )
    .force_baseline()
    .expect_ownership_helper_counts(0, 0)
    .assert_same();
}

#[test]
fn tail_call_returns_the_callee_result_with_interpreter_ownership() {
    differential(
        "function f(g,a){ return g(a) }",
        "(()=>{const events=[];const value=f(x=>{events.push('call');return {answer:x+1}},41);return [value.answer,events]})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("tail_call")
    .expect_helper(HelperId::Call)
    .assert_same();

    differential(
        "function f(g,a){ if (a) return g(a); return 0 }",
        "[f(x=>x*2,21),f(x=>x*2,0)]",
    )
    .force_baseline()
    .expect_executed_opcode("tail_call")
    .assert_same();
}

#[test]
fn tail_call_method_binds_this_and_propagates_exceptions() {
    differential(
        "function f(o,a){ return o.m(a) }",
        "(()=>{const events=[];const value=f({base:20,m(x){events.push('call');return {answer:this.base+x}}},22);return [value.answer,events]})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("tail_call_method")
    .expect_helper(HelperId::Call)
    .assert_same();

    differential(
        "function f(o,a){ return o.m(a) }",
        "(()=>{try{return f({m(){throw new Error('tail boom')}},1)}catch(error){return error.message}})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("tail_call_method")
    .assert_same();
}

#[test]
fn tail_call_exceptions_propagate_through_the_tail_called_function() {
    differential(
        "function f(g,a){ return g(a) }",
        "(()=>{try{return f(()=>{throw new Error('tail boom')},{marker:1})}catch(error){return error.message}})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("tail_call")
    .assert_same();

    differential(
        "function f(g,a){ return g(a) }",
        "(()=>{try{return f(42,1)}catch(error){return error.name}})()",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("tail_call")
    .assert_same();
}

#[test]
fn comparison_edges_match_the_interpreter_for_numbers_and_coercions() {
    for op in ["<", "<=", ">", ">=", "==", "!=", "===", "!=="] {
        let definition = format!("function f(a,b){{ return a {op} b }}");
        differential(
            &definition,
            "[f(NaN,1),f(1,NaN),f(NaN,NaN),f(-0,0),f(0,-0),f(1,1.5),f(1.5,1),f(2147483647,2147483648),f(-2147483648,-2147483649),f(1,1.0),f(Infinity,Infinity),f(-Infinity,1)]",
        )
        .force_baseline()
        .expect_ownership_helper_counts(0, 0)
        .assert_same();

        differential(
            &definition,
            "(()=>{const events=[];const value=f({[Symbol.toPrimitive](){events.push('left');return 20}},{[Symbol.toPrimitive](){events.push('right');return 22}});return [value,events,f([],''),f(null,undefined),f(null,0),f(undefined,0),f('10','9'),f('a','b'),f('1',1),f(1n,1),f(1n,1.5),f({},{})]})()",
        )
        .force_baseline()
        .stress_gc()
        .assert_same();
    }
}

#[test]
fn thrown_comparison_coercion_preserves_left_to_right_event_order() {
    differential(
        r#"
        const events=[];
        function f(left, right){ return left <= right }
        "#,
        r#"(() => {
            try {
                f(
                    { [Symbol.toPrimitive]() { events.push('left'); return 20 } },
                    { [Symbol.toPrimitive]() { events.push('right'); throw new Error('boom') } }
                );
            } catch (error) {
                return { message: error.message, events };
            }
        })()"#,
    )
    .force_baseline()
    .expect_executed_opcode("lte")
    .expect_helper(HelperId::CompareSlow)
    .assert_same();
}

#[test]
fn shift_and_bitwise_edges_match_to_int32_semantics() {
    differential(
        "function f(a,b){ return [a<<b, a>>b, a>>>b, a^b, a&b, a|b] }",
        "[f(1,31),f(1,32),f(1,33),f(-1,-1),f(1.5,1),f(3,2.7),f(NaN,1),f(2147483648,0),f(1,4294967297),f(-8,1),f(-1,32),f(-2147483648,31),f(4294967295,1),f(-1.5,0),f(-8,28),f(-1,0),f(-2147483649,0),f(1e21,3),f(-0,0),f(Infinity,1)]",
    )
    .force_baseline()
    .assert_same();

    differential(
        "function f(a){ return ~a }",
        "[f(0),f(-1),f(1.5),f(-1.5),f(4294967296.5),f(NaN),f(2147483648),f(-2147483649),f(Infinity),f(-Infinity),f(-0),f(1e21),f(2**53+2)]",
    )
    .force_baseline()
    .expect_executed_opcode("not")
    .assert_same();

    differential(
        "function f(a,b){ return a>>>b }",
        "[f(-1,0),f(-1,1),f(-2147483648,0),f(2147483648,0),f(0,0),f(-0,0),f(4294967295,0)]",
    )
    .force_baseline()
    .expect_executed_opcode("shr")
    .assert_same();
}

#[test]
fn negation_increment_and_decrement_cross_int32_and_zero_edges() {
    differential(
        "function f(a){ return -a }",
        "[f(42),Object.is(f(0),-0),Object.is(f(-0),0),f(-2147483648),f(2147483647),f(1.5),Number.isNaN(f(NaN)),f(-Infinity)]",
    )
    .force_baseline()
    .expect_executed_opcode("neg")
    .assert_same();

    differential(
        "function f(a){ var x=a; var y=a; return [++x, --y, x++, y--, x, y] }",
        "[f(2147483647),f(-2147483648),f(0),f(1.5),f(-0.5),f(NaN).map(Number.isNaN),f(Infinity)]",
    )
    .force_baseline()
    .expect_executed_opcode("post_dec")
    .assert_same();

    differential(
        "function f(a,n){ var i=a; var j=a; var s=a; while(n>0){ i++; j--; s+=n; n--; } return [i,j,s] }",
        "[f(0,3),f(2147483646,2),f(-2147483647,2),f(0.5,2),f(NaN,1).map(Number.isNaN),f(-1,1).map(v=>Object.is(v,0))]",
    )
    .force_baseline()
    .expect_executed_opcode("inc_loc")
    .expect_executed_opcode("dec_loc")
    .expect_executed_opcode("add_loc")
    .assert_same();
}

#[test]
fn empty_string_literal_is_the_owned_empty_atom() {
    differential(
        "function f(){ return '' }",
        "[f(),f().length,f()==='',f()+'x',typeof f()]",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("push_empty_string")
    .expect_helper(HelperId::AtomValue)
    .assert_same();
}

#[test]
fn is_undefined_and_is_null_release_their_operand() {
    differential(
        "function f(a){ return a === void 0 }",
        "[f(undefined),f(null),f(0),f(''),f({}),f([]),f(NaN),f(false)]",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("is_undefined")
    .expect_helper(HelperId::Free)
    .assert_same();

    differential(
        "function f(a){ return a === null }",
        "[f(null),f(undefined),f(0),f(''),f({}),f([]),f(NaN),f(false)]",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("is_null")
    .expect_helper(HelperId::Free)
    .assert_same();

    differential(
        "function f(a){ if (a === null) return 'null'; if (a === void 0) return 'undefined'; return 'value' }",
        "[f(null),f(undefined),f({})]",
    )
    .force_baseline()
    .stress_gc()
    .assert_same();
}

#[test]
fn native_numeric_ops_deoptimize_instead_of_reinterpreting_non_numeric_operands() {
    // Values with an unknown entry domain (globals, elements, property
    // values, call results) reach natively lowered arithmetic through a
    // use-site tag guard; a failed guard resumes in the interpreter at that
    // instruction with the exact coercion semantics.
    differential(
        "globalThis.g='5'; function f(){ return [g*2, g-1, -g, ~g, g<<1, g>>>0, g^1, g/2] }",
        "f()",
    )
    .force_baseline()
    .expect_deopt()
    .assert_same();

    differential(
        "function f(a,i){ return a[i] * 2 }",
        "[f(['5'],0),f([21],0),f([{valueOf(){return 4}}],0)]",
    )
    .force_baseline()
    .expect_deopt()
    .assert_same();

    differential(
        "function f(o){ return o.p++ }",
        "(()=>{const s={p:'41'};const n={p:41};const b={p:1n};return [f(s),s.p,f(n),n.p,String(f(b)),String(b.p)]})()",
    )
    .force_baseline()
    .expect_executed_opcode("perm3")
    .expect_deopt()
    .assert_same();

    // An argument root with a non-numeric value retries before entry; a
    // property value only reaches `inc_loc` through the use-site guard.
    differential(
        "function f(o,n){ var i=o.v; while(n>0){ i++; n--; } return i }",
        "[f({v:'1'},2),f({v:1},2),f({v:2147483647},1)]",
    )
    .force_baseline()
    .expect_executed_opcode("inc_loc")
    .expect_deopt()
    .assert_same();

    differential(
        "function f(g){ var x=g(); return x - 1 }",
        "(()=>{try{return [f(()=>'z'),f(()=>2),String(f(()=>3n))]}catch(error){return error.name}})()",
    )
    .force_baseline()
    .expect_deopt()
    .assert_same();

    differential(
        "function f(g){ return g() * 2 }",
        "(()=>{try{return f(()=>Symbol())}catch(error){return error.name}})()",
    )
    .force_baseline()
    .expect_deopt()
    .assert_same();
}

#[test]
fn keeping_set_local_and_set_argument_replace_primitive_slots_with_owned_values() {
    // The DUP helper rejects an occupied destination; a primitive previous
    // value is released without a FREE helper, so the keep paths must clear
    // the slot before duplicating a refcounted replacement into it.
    differential(
        "function f(a){ var x=0; return x=a }",
        "[f({answer:42}).answer,f('s'),f(7),f(null)]",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("set_loc0")
    .assert_same();

    differential(
        "function f(a,v){ return a=v }",
        "[f(0,42),f(0,{answer:42}).answer,f({old:1},'s'),f('old',null)]",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("set_arg0")
    .assert_same();

    differential(
        "function f(a,b,c,d,e,v){ e=v; return e=v }",
        "[f(0,0,0,0,0,{answer:42}).answer,f(0,0,0,0,{old:1},[1,2]).length]",
    )
    .force_baseline()
    .stress_gc()
    .expect_executed_opcode("put_arg")
    .expect_executed_opcode("set_arg")
    .assert_same();
}

#[test]
fn property_values_used_numerically_enter_native_code_without_retry() {
    differential(
        "function f(o){ return o.length - 1 }",
        "[f([1,2,3]),f({length:2.5})]",
    )
    .force_baseline()
    .expect_executed_opcode("sub")
    .assert_same();
}

#[test]
fn float64_fast_add_preserves_nan_negative_zero_and_infinity() {
    differential(
        "function f(x, y) { return x + y; }",
        "[Number.isNaN(f(NaN, 1.5)), Object.is(f(-0, -0), -0), f(Infinity, 2.5), f(1.25, 2.5)]",
    )
    .force_baseline()
    .expect_ownership_helper_counts(0, 0)
    .assert_same();
}

#[test]
fn modulo_uses_the_exact_slow_path_outside_the_int32_fast_path() {
    differential(
        "function f(a,b){ return a % b }",
        "[f(7,3),f(-7,3),f(7,-3),Object.is(f(-4,2),-0),Object.is(f(4,-2),0),Number.isNaN(f(5,0)),Number.isNaN(f(-5,0)),f(5,Infinity),f(-2147483648,-1),f(2147483648,7),f(5.5,2),f(-5.5,2),f(1e308,1e-308),Number.isNaN(f(Infinity,2)),Number.isNaN(f(NaN,2)),f(0,5),Object.is(f(-0,5),-0)]",
    )
    .force_baseline()
    .expect_executed_opcode("mod")
    .expect_helper(HelperId::BinaryArithSlow)
    .stress_gc()
    .assert_same();

    differential(
        "function f(a,b){ return a % b }",
        "(()=>{const events=[];const r=f({[Symbol.toPrimitive](){events.push('left');return 20}},{[Symbol.toPrimitive](){events.push('right');return 6}});return [r,events,f('10',3),String(f(7n,3n)),f(true,2),f(null,2)]})()",
    )
    .force_baseline()
    .expect_executed_opcode("mod")
    .expect_helper(HelperId::BinaryArithSlow)
    .stress_gc()
    .assert_same();

    differential(
        "function f(a,b){ return a % b }",
        "(()=>{try{return f(7n,3)}catch(e){return e.name}})()+'|'+(()=>{try{return f({valueOf(){throw new RangeError('x')}},3)}catch(e){return e.name}})()+'|'+(()=>{try{return f(7n,0n)}catch(e){return e.name}})()",
    )
    .force_baseline()
    .expect_executed_opcode("mod")
    .expect_helper(HelperId::BinaryArithSlow)
    .assert_same();

    differential(
        "function f(n){ let s=0; for(let i=0;i<n;i++){ s = s + (i % 7); } return s }",
        "f(100)",
    )
    .force_baseline()
    .expect_ownership_helper_counts(0, 0)
    .assert_same();
}

#[test]
fn unary_arithmetic_uses_the_exact_slow_path_for_non_numeric_operands() {
    differential(
        "function f(a){ var x=a; return [-a, ~a, ++x, --x] }",
        "(()=>{const events=[];const r=f({[Symbol.toPrimitive](){events.push('p');return 6}});return [r,events,f('7'),f(null),f(true),f(undefined).map(Number.isNaN),String(f(5n))]})()",
    )
    .force_baseline()
    .expect_executed_opcode("neg")
    .expect_helper(HelperId::UnaryArithSlow)
    .stress_gc()
    .assert_same();

    differential(
        "function f(a){ return -a }",
        "(()=>{try{return f(Symbol())}catch(e){return e.name}})()+'|'+(()=>{try{return f({valueOf(){throw new RangeError('x')}})}catch(e){return e.name}})()",
    )
    .force_baseline()
    .expect_executed_opcode("neg")
    .expect_helper(HelperId::UnaryArithSlow)
    .assert_same();

    differential(
        "function f(a){ return [-a, ~a] }",
        "[f(42),f(0).map(v=>Object.is(v,-0)),f(-2147483648),f(1.5),f(NaN).map(Number.isNaN)]",
    )
    .force_baseline()
    .expect_ownership_helper_counts(0, 0)
    .assert_same();
}
