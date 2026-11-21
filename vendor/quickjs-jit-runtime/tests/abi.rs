use rquickjs_jit::abi::{AbiInfo, ABI_MAJOR, ABI_MINOR};

const BUNDLED_TARGETS: [&str; 9] = [
    "x86_64-unknown-linux-gnu.rs",
    "aarch64-unknown-linux-gnu.rs",
    "x86_64-unknown-linux-musl.rs",
    "aarch64-unknown-linux-musl.rs",
    "x86_64-apple-darwin.rs",
    "aarch64-apple-darwin.rs",
    "x86_64-pc-windows-gnu.rs",
    "x86_64-pc-windows-msvc.rs",
    "aarch64-pc-windows-msvc.rs",
];

fn try_jit_declarations(source: &str) -> Option<String> {
    let lines: Vec<_> = source.lines().collect();
    let function_id = lines
        .iter()
        .position(|line| line.trim_start().starts_with("pub struct JSJitFunctionId"));
    let array_mode = lines.iter().position(|line| {
        line.trim_start()
            .starts_with("pub const JSJitArrayMode_JS_JIT_ARRAY_MODE_GENERIC")
    });
    let last_function = lines
        .iter()
        .position(|line| line.trim_start().starts_with("pub fn JS_JitGetInlineAPI"));
    match (function_id, array_mode, last_function) {
        (None, None, None) => return None,
        (Some(_), Some(_), Some(_)) => {}
        _ => {
            panic!("fresh bindgen output contains a partial JIT ABI declaration set")
        }
    }
    let function_id = function_id.unwrap();
    let array_mode = array_mode.unwrap();
    let last_function = last_function.unwrap();
    let first_declaration = array_mode.min(
        function_id
            .checked_sub(2)
            .expect("attributes before JSJitFunctionId"),
    );
    let declarations_end = lines[last_function..]
        .iter()
        .position(|line| line.trim() == "}")
        .map(|offset| last_function + offset + 1)
        .expect("end of JS_JitGetInlineAPI extern block");
    assert!(
        first_declaration < last_function,
        "JIT declarations must retain their canonical order"
    );
    let mut normalized = String::from("pub type size_t = NORMALIZED;\n");
    for line in &lines {
        let line = line.trim_start();
        if line.starts_with("pub const QJSJIT_ABI_")
            || line.starts_with("pub const QJSJIT_ARRAY_API_VERSION")
            || line.starts_with("pub const QJSJIT_INLINE_")
            || line.starts_with("pub const JS_JIT_INLINE_")
            || line.starts_with("pub const JS_JIT_FEEDBACK_")
        {
            normalized.push_str(line);
            normalized.push('\n');
        }
    }
    for line in &lines[first_declaration..declarations_end] {
        normalized.push_str(line.trim_start());
        normalized.push('\n');
    }
    Some(normalized)
}

fn jit_declarations(source: &str) -> String {
    try_jit_declarations(source).expect("complete JIT ABI declarations")
}

#[test]
fn jit_declaration_extraction_does_not_depend_on_atom_bindings() {
    let canonical = bundled_binding(BUNDLED_TARGETS[0]);
    let without_atoms = canonical
        .lines()
        .take_while(|line| !line.starts_with("pub const __JS_ATOM_NULL"))
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(
        jit_declarations(&canonical),
        jit_declarations(&without_atoms)
    );

    let indented = canonical
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(jit_declarations(&canonical), jit_declarations(&indented));
}

#[test]
fn fresh_binding_classification_rejects_partial_jit_abi_output() {
    assert!(try_jit_declarations("pub struct JSRuntime;").is_none());
    let partial =
        std::panic::catch_unwind(|| try_jit_declarations("pub struct JSJitFunctionId {}"));
    assert!(partial.is_err());
}

#[test]
fn binding_classification_rejects_a_missing_inline_query() {
    let canonical = bundled_binding(BUNDLED_TARGETS[0]);
    let truncated = canonical.replace("pub fn JS_JitGetInlineAPI", "pub fn MissingInlineAPI");
    assert!(std::panic::catch_unwind(|| try_jit_declarations(&truncated)).is_err());
}

#[test]
fn binding_comparison_includes_inline_table_fields_and_constants() {
    let canonical = bundled_binding(BUNDLED_TARGETS[0]);
    let changed_field = canonical.replace("pub max_depth: u32", "pub renamed_depth: u32");
    let changed_constant = canonical.replace(
        "pub const JS_JIT_INLINE_MAX_DEPTH: u32 = 16;",
        "pub const JS_JIT_INLINE_MAX_DEPTH: u32 = 15;",
    );
    assert!(
        jit_declarations(&canonical) != jit_declarations(&changed_field),
        "inline table field changes must affect cross-target comparison"
    );
    assert!(
        jit_declarations(&canonical) != jit_declarations(&changed_constant),
        "inline recovery constant changes must affect cross-target comparison"
    );
}

#[test]
fn binding_comparison_includes_inline_check_and_array_feedback_flags() {
    let canonical = bundled_binding(BUNDLED_TARGETS[0]);
    for (original, replacement) in [
        ("pub fn JS_JitInlineCheck", "pub fn MissingInlineCheck"),
        ("pub check:", "pub missing_check:"),
        (
            "pub const JS_JIT_FEEDBACK_ARRAY_STORE: u32 = 64;",
            "pub const JS_JIT_FEEDBACK_ARRAY_STORE: u32 = 32;",
        ),
    ] {
        let changed = canonical.replace(original, replacement);
        assert_ne!(canonical, changed, "fixture must change: {original}");
        assert!(
            jit_declarations(&canonical) != jit_declarations(&changed),
            "ABI comparison must include {original}"
        );
    }
}

#[test]
fn binding_comparison_includes_array_api_version() {
    let canonical = bundled_binding(BUNDLED_TARGETS[0]);
    let normalized = jit_declarations(&canonical);
    assert!(normalized.contains("pub const QJSJIT_ARRAY_API_VERSION: u32 = 1;"));

    let changed = canonical.replace(
        "pub const QJSJIT_ARRAY_API_VERSION: u32 = 1;",
        "pub const QJSJIT_ARRAY_API_VERSION: u32 = 2;",
    );
    assert_ne!(
        canonical, changed,
        "fixture must change the array API version"
    );
    assert_ne!(
        normalized,
        jit_declarations(&changed),
        "array API version changes must affect cross-target comparison"
    );
}

#[test]
fn binding_classification_includes_array_declarations_before_function_id() {
    let reordered = r#"
pub const JSJitArrayMode_JS_JIT_ARRAY_MODE_GENERIC: JSJitArrayMode = 0;
pub type JSJitArrayMode = ::core::ffi::c_uint;
pub type JSJitArrayQueryStatus = ::core::ffi::c_int;
#[repr(C)]
pub struct JSJitArrayMetadata {
    pub data: *mut ::core::ffi::c_void,
}
pub type JSJitArrayQueryFunc = ::core::option::Option<unsafe extern "C" fn()>;
#[repr(C)]
pub struct JSJitArrayAPI {
    pub query: JSJitArrayQueryFunc,
}
unsafe extern "C" {
    pub fn JS_JitGetArrayAPI(version: u32) -> *const JSJitArrayAPI;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct JSJitFunctionId {}
unsafe extern "C" {
    pub fn JS_JitGetInlineAPI(version: u32) -> *const JSJitInlineAPI;
}
"#;

    let normalized = jit_declarations(reordered);
    assert!(normalized.contains("pub type JSJitArrayMode = ::core::ffi::c_uint;"));
    assert!(normalized.contains("pub fn JS_JitGetArrayAPI(version: u32)"));
}

#[test]
fn binding_comparison_covers_every_array_abi_declaration() {
    let canonical = bundled_binding(BUNDLED_TARGETS[0]);
    let normalized = jit_declarations(&canonical);
    for (original, replacement) in [
        (
            "pub type JSJitArrayMode = ::core::ffi::c_uint;",
            "pub type JSJitArrayMode = ::core::ffi::c_int;",
        ),
        (
            "pub const JSJitArrayMode_JS_JIT_ARRAY_MODE_PACKED: JSJitArrayMode = 1;",
            "pub const JSJitArrayMode_JS_JIT_ARRAY_MODE_PACKED: JSJitArrayMode = 2;",
        ),
        (
            "pub type JSJitArrayQueryStatus = ::core::ffi::c_int;",
            "pub type JSJitArrayQueryStatus = ::core::ffi::c_uint;",
        ),
        (
            "pub const JSJitArrayQueryStatus_JS_JIT_ARRAY_QUERY_OK: JSJitArrayQueryStatus = 1;",
            "pub const JSJitArrayQueryStatus_JS_JIT_ARRAY_QUERY_OK: JSJitArrayQueryStatus = 2;",
        ),
        (
            "pub struct JSJitArrayMetadata",
            "pub struct ChangedArrayMetadata",
        ),
        (
            "pub type JSJitArrayQueryFunc =",
            "pub type ChangedArrayQueryFunc =",
        ),
        ("pub struct JSJitArrayAPI", "pub struct ChangedArrayAPI"),
        (
            "pub fn JS_JitGetArrayAPI(version: u32) -> *const JSJitArrayAPI;",
            "pub fn JS_JitGetArrayAPI(version: u32) -> *mut JSJitArrayAPI;",
        ),
    ] {
        let changed = canonical.replace(original, replacement);
        assert_ne!(canonical, changed, "fixture must change: {original}");
        assert_ne!(
            normalized,
            jit_declarations(&changed),
            "ABI comparison must include {original}"
        );
    }
}

fn bundled_binding(target: &str) -> String {
    let binding_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../sys/src/bindings");
    std::fs::read_to_string(binding_dir.join(target)).unwrap()
}

#[test]
#[allow(clippy::absurd_extreme_comparisons)]
fn linked_abi_matches_rust_contract() {
    let info = AbiInfo::linked().expect("ABI info");
    assert_eq!(info.major(), ABI_MAJOR);
    assert!(info.minor() >= ABI_MINOR);
    assert_eq!(info.pointer_width(), usize::BITS as u8);
    assert_eq!(info.little_endian(), cfg!(target_endian = "little"));
}

#[test]
fn abi_minor_additions_are_appended_after_the_v1_0_tail() {
    use rquickjs_core::qjs;

    let previous_tail = std::mem::offset_of!(qjs::JSJitABIInfo, backend_vtable_layout_fingerprint)
        + std::mem::size_of::<u64>();
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitABIInfo, exec_frame_layout_fingerprint),
        previous_tail
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitABIInfo, exit_layout_fingerprint),
        previous_tail + std::mem::size_of::<u64>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitABIInfo, runtime_api_layout_fingerprint),
        previous_tail + 2 * std::mem::size_of::<u64>()
    );
}

#[repr(C)]
struct AbiInfoV1_0 {
    struct_size: u32,
    major: u16,
    minor: u16,
    pointer_width: u8,
    little_endian: u8,
    value_size: u16,
    source_revision: u64,
    opcode_fingerprint: u64,
    value_layout_fingerprint: u64,
    build_feature_flags: u64,
    build_fingerprint: u64,
    abi_info_layout_fingerprint: u64,
    function_id_layout_fingerprint: u64,
    hot_event_layout_fingerprint: u64,
    function_snapshot_layout_fingerprint: u64,
    entry_handle_layout_fingerprint: u64,
    backend_vtable_layout_fingerprint: u64,
}

#[repr(C)]
struct AbiInfoV1_1 {
    prefix: AbiInfoV1_0,
    exec_frame_layout_fingerprint: u64,
    exit_layout_fingerprint: u64,
}

#[repr(C)]
struct AbiInfoV1_2 {
    prefix: AbiInfoV1_1,
    runtime_api_layout_fingerprint: u64,
}

#[repr(C)]
struct Guarded<T> {
    prefix: T,
    canary: [u8; 16],
}

#[test]
fn linked_abi_query_fills_every_old_prefix_without_touching_canaries() {
    use rquickjs_core::qjs;

    assert_eq!(
        std::mem::size_of::<AbiInfoV1_0>(),
        std::mem::offset_of!(qjs::JSJitABIInfo, exec_frame_layout_fingerprint)
    );
    assert_eq!(
        std::mem::size_of::<AbiInfoV1_1>(),
        std::mem::offset_of!(qjs::JSJitABIInfo, runtime_api_layout_fingerprint)
    );

    let mut v1_0: Guarded<AbiInfoV1_0> = unsafe { std::mem::zeroed() };
    v1_0.prefix.struct_size = std::mem::size_of::<AbiInfoV1_0>() as u32;
    v1_0.canary = [0xa5; 16];
    let status = unsafe {
        qjs::JS_GetJitABIInfo((&mut v1_0.prefix as *mut AbiInfoV1_0).cast::<qjs::JSJitABIInfo>())
    };
    assert_eq!(status, qjs::JS_JIT_BACKEND_OK);
    assert_eq!(
        v1_0.prefix.struct_size as usize,
        std::mem::size_of::<qjs::JSJitABIInfo>()
    );
    assert_eq!(v1_0.prefix.major, ABI_MAJOR);
    assert_eq!(v1_0.prefix.minor, ABI_MINOR);
    assert_eq!(v1_0.canary, [0xa5; 16]);

    let mut v1_1: Guarded<AbiInfoV1_1> = unsafe { std::mem::zeroed() };
    v1_1.prefix.prefix.struct_size = std::mem::size_of::<AbiInfoV1_1>() as u32;
    v1_1.canary = [0x5a; 16];
    let status = unsafe {
        qjs::JS_GetJitABIInfo((&mut v1_1.prefix as *mut AbiInfoV1_1).cast::<qjs::JSJitABIInfo>())
    };
    assert_eq!(status, qjs::JS_JIT_BACKEND_OK);
    assert_eq!(
        v1_1.prefix.prefix.struct_size as usize,
        std::mem::size_of::<qjs::JSJitABIInfo>()
    );
    assert_eq!(v1_1.prefix.prefix.major, ABI_MAJOR);
    assert_eq!(v1_1.prefix.prefix.minor, ABI_MINOR);
    assert_eq!(v1_1.canary, [0x5a; 16]);

    assert_eq!(
        std::mem::size_of::<AbiInfoV1_2>(),
        std::mem::offset_of!(qjs::JSJitABIInfo, helper_table_fingerprint)
    );
    let mut v1_2: Guarded<AbiInfoV1_2> = unsafe { std::mem::zeroed() };
    v1_2.prefix.prefix.prefix.struct_size = std::mem::size_of::<AbiInfoV1_2>() as u32;
    v1_2.canary = [0x3c; 16];
    let status = unsafe {
        qjs::JS_GetJitABIInfo((&mut v1_2.prefix as *mut AbiInfoV1_2).cast::<qjs::JSJitABIInfo>())
    };
    assert_eq!(status, qjs::JS_JIT_BACKEND_OK);
    assert_eq!(
        v1_2.prefix.prefix.prefix.struct_size as usize,
        std::mem::size_of::<qjs::JSJitABIInfo>()
    );
    assert_eq!(v1_2.prefix.prefix.prefix.minor, ABI_MINOR);
    assert_eq!(v1_2.canary, [0x3c; 16]);
}

#[test]
fn interrupt_runtime_api_is_a_versioned_exec_frame_tail_extension() {
    use rquickjs_core::qjs;

    assert_eq!(qjs::QJSJIT_RUNTIME_API_MAJOR, 1);
    assert_eq!(qjs::QJSJIT_RUNTIME_API_MINOR, 10);
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitExecFrame, runtime_api),
        std::mem::offset_of!(qjs::JSJitExecFrame, entry)
            + std::mem::size_of::<qjs::JSJitEntryHandle>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitRuntimeAPI, interrupt_poll),
        8
    );
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, shape_guard), 112);
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitRuntimeAPI, materialize_owner),
        120
    );
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, get_element), 128);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, set_element), 136);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, to_propkey), 144);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, get_global), 152);
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitRuntimeAPI, call_constructor),
        160
    );
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, regexp), 168);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, atom_value), 176);
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitRuntimeAPI, binary_arith_slow),
        184
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitRuntimeAPI, unary_arith_slow),
        192
    );
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, fclosure), 200);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, get_var_ref), 208);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, put_var_ref), 216);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, close_loc), 224);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, set_name), 232);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, generic_op), 240);
    assert_eq!(std::mem::offset_of!(qjs::JSJitRuntimeAPI, iterator_op), 248);
    assert_eq!(std::mem::size_of::<qjs::JSJitRuntimeAPI>(), 256);
    assert_eq!(qjs::JS_JIT_HELPER_GUARD_MISS, 1);
}

#[test]
fn helper_abi_is_one_canonical_versioned_table_in_c_bindgen_and_rust() {
    use std::ffi::CStr;

    use rquickjs_core::qjs;

    let expected = [
        ("POLL", 0_u8, 0_u8),
        (
            "DUP",
            2,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        ("FREE", 1, 0),
        (
            "RESOLVE_CONST",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "TO_NUMERIC",
            2,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "TO_BOOL",
            2,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "ADD_SLOW",
            3,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "COMPARE_SLOW",
            3,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "GET_PROPERTY",
            2,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        ("SET_PROPERTY", 2, 0),
        (
            "CALL",
            4,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "NEW_ARRAY",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "NEW_OBJECT",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        ("SHAPE_GUARD", 1, 0),
        (
            "MATERIALIZE_OWNER",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "GET_ELEMENT",
            3,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        ("SET_ELEMENT", 3, 0),
        (
            "TO_PROPKEY",
            2,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "GET_GLOBAL",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "CALL_CONSTRUCTOR",
            4,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "REGEXP",
            3,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "ATOM_VALUE",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "BINARY_ARITH_SLOW",
            3,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "UNARY_ARITH_SLOW",
            2,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "FCLOSURE",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "GET_VAR_REF",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "PUT_VAR_REF",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_NONE as u8,
        ),
        (
            "CLOSE_LOC",
            0,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_NONE as u8,
        ),
        (
            "SET_NAME",
            1,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_NONE as u8,
        ),
        (
            "GENERIC_OP",
            3,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_OWNED as u8,
        ),
        (
            "ITERATOR_OP",
            0,
            qjs::JSJitHelperOwnership_JS_JIT_HELPER_NONE as u8,
        ),
    ];

    let mut count = 0_u32;
    let mut fingerprint = 0_u64;
    let native = unsafe { qjs::JS_JitGetHelperTable(&mut count, &mut fingerprint) };
    assert!(!native.is_null());
    assert_eq!(count as usize, expected.len());
    assert_eq!(count as usize, qjs::QJSJIT_GENERATED_HELPERS.len());
    assert_eq!(fingerprint, qjs::QJSJIT_GENERATED_HELPER_FINGERPRINT);

    for (index, ((name, value_arity, output_ownership), generated)) in expected
        .iter()
        .zip(qjs::QJSJIT_GENERATED_HELPERS)
        .enumerate()
    {
        let native = unsafe { &*native.add(index) };
        assert_eq!(native.id as usize, index);
        assert_eq!(generated.id as usize, index);
        assert_eq!(
            unsafe { CStr::from_ptr(native.name) }.to_str().unwrap(),
            *name
        );
        assert_eq!(generated.name, *name);
        assert_eq!(native.value_arity, *value_arity);
        assert_eq!(generated.value_arity, *value_arity);
        assert_eq!(native.output_ownership, *output_ownership);
        assert_eq!(generated.output_ownership, *output_ownership);
        assert_eq!(native.flags, generated.flags);
        assert_eq!(native.abi_type_count, generated.abi_types.len() as u8);
        assert_eq!(
            &native.abi_types[..usize::from(native.abi_type_count)],
            generated.abi_types
        );
        assert_eq!(
            &native.value_ownership[..usize::from(native.value_arity)],
            generated.value_ownership
        );
    }
}

#[test]
fn helper_abi_fields_are_append_only_tails() {
    use rquickjs_core::qjs;

    assert_eq!(qjs::QJSJIT_ABI_MINOR, 25);
    assert_eq!(qjs::QJSJIT_RUNTIME_API_MAJOR, 1);
    assert_eq!(qjs::QJSJIT_RUNTIME_API_MINOR, 10);
    assert_eq!(qjs::QJSJIT_HELPER_ABI_VERSION, 1);
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitEntryHandle, stack_map_count),
        std::mem::offset_of!(qjs::JSJitEntryHandle, pin)
            + std::mem::size_of::<*mut core::ffi::c_void>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitEntryHandle, helper_abi_version),
        std::mem::offset_of!(qjs::JSJitEntryHandle, stack_map_count) + 4
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitExecFrame, runtime_id),
        std::mem::offset_of!(qjs::JSJitExecFrame, runtime_api)
            + std::mem::size_of::<*const qjs::JSJitRuntimeAPI>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitExecFrame, frame_cookie),
        std::mem::offset_of!(qjs::JSJitExecFrame, runtime_id) + 8
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitExecFrame, stack_capacity),
        std::mem::offset_of!(qjs::JSJitExecFrame, frame_cookie) + 8
    );
    assert_eq!(qjs::JS_JIT_HELPER_SCRATCH_SLOTS, 2);
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitBackendVTable, native_enter),
        std::mem::offset_of!(qjs::JSJitBackendVTable, memory_used) + std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitBackendVTable, native_exit),
        std::mem::offset_of!(qjs::JSJitBackendVTable, native_enter) + std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitBackendVTable, record_feedback),
        std::mem::offset_of!(qjs::JSJitBackendVTable, native_exit) + std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitABIInfo, helper_table_fingerprint),
        std::mem::offset_of!(qjs::JSJitABIInfo, runtime_api_layout_fingerprint) + 8
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitABIInfo, element_layout_fingerprint),
        std::mem::offset_of!(qjs::JSJitABIInfo, helper_table_fingerprint) + 8
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitABIInfo, element_layout),
        std::mem::offset_of!(qjs::JSJitABIInfo, element_layout_fingerprint) + 8
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitElementLayout, array_buffer_immutable_offset),
        std::mem::offset_of!(qjs::JSJitElementLayout, array_buffer_detached_offset) + 4
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitElementLayout, array_class_id),
        std::mem::offset_of!(qjs::JSJitElementLayout, array_buffer_immutable_offset) + 4
    );
}

#[test]
fn inline_api_has_an_exact_versioned_header_and_native_pointer_tail() {
    use rquickjs_core::qjs;
    use std::mem::{align_of, offset_of, size_of};

    // Version one consists of six u32 header fields followed by exactly four
    // native operation pointers. It extends the exported ABI without adding
    // fields to the existing execution frame or generated helper table.
    let pointer_size = size_of::<usize>();
    assert_eq!(align_of::<qjs::JSJitInlineAPI>(), align_of::<usize>());
    assert_eq!(size_of::<qjs::JSJitInlineAPI>(), 24 + 4 * pointer_size);
    assert_eq!(
        [
            offset_of!(qjs::JSJitInlineAPI, struct_size),
            offset_of!(qjs::JSJitInlineAPI, version),
            offset_of!(qjs::JSJitInlineAPI, max_depth),
            offset_of!(qjs::JSJitInlineAPI, max_bytes),
            offset_of!(qjs::JSJitInlineAPI, effects),
            offset_of!(qjs::JSJitInlineAPI, reserved),
        ],
        [0, 4, 8, 12, 16, 20]
    );
    assert_eq!(offset_of!(qjs::JSJitInlineAPI, enter), 24);
    assert_eq!(
        offset_of!(qjs::JSJitInlineAPI, check),
        24 + 3 * pointer_size
    );
    assert_eq!(offset_of!(qjs::JSJitInlineAPI, leave), 24 + pointer_size);
    assert_eq!(
        offset_of!(qjs::JSJitInlineAPI, resume),
        24 + 2 * pointer_size
    );

    let info = AbiInfo::linked().unwrap();
    assert!(info.minor() >= 23);
    let pointer = unsafe { qjs::JS_JitGetInlineAPI(1) };
    assert!(!pointer.is_null());
    // SAFETY: The successful versioned query returns immutable static storage.
    // Check the header size before reading the operation-pointer tail.
    assert_eq!(
        unsafe { core::ptr::addr_of!((*pointer).struct_size).read() } as usize,
        size_of::<qjs::JSJitInlineAPI>()
    );
    let api = unsafe { &*pointer };
    assert_eq!(api.version, 1);
    assert_eq!(api.max_depth, 16);
    assert_eq!(api.max_bytes, 1024 * 1024);
    assert_eq!(api.reserved, 0);
    assert_eq!(
        api.effects,
        qjs::JS_JIT_HELPER_THROWING
            | qjs::JS_JIT_HELPER_ALLOCATING
            | qjs::JS_JIT_HELPER_REENTRANT
            | qjs::JS_JIT_HELPER_FINALIZING
    );
    assert_eq!(
        api.enter.unwrap() as usize,
        qjs::JS_JitInlineEnter as *const () as usize
    );
    assert_eq!(
        api.leave.unwrap() as usize,
        qjs::JS_JitInlineLeave as *const () as usize
    );
    assert_eq!(
        api.resume.unwrap() as usize,
        qjs::JS_JitInlineResume as *const () as usize
    );
    assert_eq!(
        api.check.unwrap() as usize,
        qjs::JS_JitInlineCheck as *const () as usize
    );
    assert!(unsafe { qjs::JS_JitGetInlineAPI(0) }.is_null());
    assert!(unsafe { qjs::JS_JitGetInlineAPI(2) }.is_null());
}

#[test]
#[cfg(feature = "test-support")]
fn backend_is_detached_before_runtime_drop() {
    let events = rquickjs_jit::test_support::record_lifecycle();
    {
        let _runtime = events.runtime();
    }
    assert_eq!(
        events.take(),
        ["attach", "detach", "backend_drop", "runtime_drop"]
    );
}

#[test]
#[cfg(feature = "test-support")]
fn cloned_runtime_outlives_the_detached_backend() {
    let events = rquickjs_jit::test_support::record_lifecycle();
    let runtime = events.runtime();
    let runtime_clone = runtime.runtime().clone();
    drop(runtime);

    assert_eq!(events.snapshot(), ["attach", "detach", "backend_drop"]);
    drop(runtime_clone);
    assert_eq!(
        events.take(),
        ["attach", "detach", "backend_drop", "runtime_drop"]
    );
}

#[test]
#[cfg(feature = "test-support")]
fn cloned_context_outlives_the_detached_backend() {
    let events = rquickjs_jit::test_support::record_lifecycle();
    let runtime = events.runtime();
    let context = rquickjs::Context::full(runtime.runtime()).unwrap();
    drop(runtime);

    assert_eq!(events.snapshot(), ["attach", "detach", "backend_drop"]);
    drop(context);
    assert_eq!(
        events.take(),
        ["attach", "detach", "backend_drop", "runtime_drop"]
    );
}

#[test]
#[cfg(feature = "test-support")]
fn duplicate_attachment_does_not_replace_the_first_backend() {
    assert!(rquickjs_jit::test_support::duplicate_attachment_is_rejected());
}

#[test]
#[cfg(feature = "test-support")]
fn every_abi_mismatch_is_rejected_before_backend_storage() {
    use rquickjs_jit::test_support::AbiMismatchFixture;

    for required in [
        AbiMismatchFixture::InlineApi,
        AbiMismatchFixture::ArrayApi,
        AbiMismatchFixture::IteratorApi,
        AbiMismatchFixture::ObjectApi,
    ] {
        assert!(
            AbiMismatchFixture::ALL.contains(&required),
            "missing integration fixture: {required:?}"
        );
    }
    for mismatch in AbiMismatchFixture::ALL {
        assert!(
            rquickjs_jit::test_support::mismatch_is_rejected_before_attach(mismatch),
            "fixture was accepted: {mismatch:?}"
        );
    }
}

#[test]
fn bundled_targets_share_jit_declarations() {
    let reference = bundled_binding(BUNDLED_TARGETS[0]);
    let reference = jit_declarations(&reference);
    for target in BUNDLED_TARGETS.iter().skip(1) {
        assert_eq!(
            jit_declarations(&bundled_binding(target)),
            reference,
            "{target}"
        );
    }
}

#[test]
#[cfg(all(feature = "compiler", feature = "test-support", feature = "bindgen"))]
fn bundled_targets_match_fresh_bindgen_output() {
    let generated = rquickjs_jit::test_support::fresh_bindgen_bindings()
        .expect("test must receive fresh bindgen output");
    // A sanitizer workspace build can select the non-JIT host instance of
    // rquickjs-sys for this observation hook. That output is not a fresh JIT
    // binding and therefore cannot be compared to the JIT-enabled bundles.
    // Partial JIT output remains a hard failure above.
    let Some(generated) = try_jit_declarations(generated) else {
        return;
    };
    for target in BUNDLED_TARGETS {
        assert_eq!(
            jit_declarations(&bundled_binding(target)),
            generated,
            "{target}"
        );
    }
}

#[test]
fn exported_property_layout_tracks_descriptor_mutation_and_preserves_value_only_stores() {
    use rquickjs::{Context, Object, Runtime};
    use rquickjs_core::qjs;

    let mut info: qjs::JSJitABIInfo = unsafe { std::mem::zeroed() };
    info.struct_size = std::mem::size_of_val(&info) as u32;
    assert_eq!(
        unsafe { qjs::JS_GetJitABIInfo(&mut info) },
        qjs::JS_JIT_BACKEND_OK
    );
    let layout = info.property_layout;
    assert_eq!(
        layout.struct_size as usize,
        std::mem::size_of::<qjs::JSJitPropertyLayout>()
    );
    assert_eq!(
        std::mem::offset_of!(qjs::JSJitABIInfo, property_layout_fingerprint),
        std::mem::offset_of!(qjs::JSJitABIInfo, element_layout)
            + std::mem::size_of::<qjs::JSJitElementLayout>()
    );
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        // No backend/artifact exists: seed an observation token to exercise the
        // C mutation contract directly, including already-unhashed shapes.
        for mutation in [
            "o.z=3",
            "for(let i=0;i<100;i++)o['new'+i]=i",
            "delete o.x",
            "Object.defineProperty(o,'x',{writable:false})",
            "Object.defineProperty(o,'x',{enumerable:false})",
            "Object.defineProperty(o,'x',{get(){return 7}})",
            "Object.setPrototypeOf(o,{inherited:8})",
            "Object.freeze(o)",
            "Object.seal(o)",
            "for(let i=0;i<20;i++)delete o['p'+i]",
        ] {
            ctx.eval::<(), _>("globalThis.o=Object.create(null);o.x=1;o.y=2;for(let i=0;i<24;i++)o['p'+i]=i;Object.defineProperty(o,'y',{enumerable:false})").unwrap();
            let object: Object = ctx.globals().get("o").unwrap();
            let raw = object.as_value().as_raw();
            let object_ptr = unsafe { raw.u.ptr.cast::<u8>() };
            let shape = unsafe {
                object_ptr.add(layout.object_shape_offset as usize).cast::<*mut u8>().read()
            };
            let generation = unsafe { shape.add(layout.shape_generation_offset as usize).cast::<u64>() };
            assert_eq!(unsafe { generation.read() }, 0);
            unsafe { generation.write(0x1234_5678_9abc_def0) };
            ctx.eval::<(), _>("o.x=9").unwrap();
            assert_eq!(unsafe { generation.read() }, 0x1234_5678_9abc_def0,
                "a primitive value-only store preserves the descriptor token");
            let properties = unsafe {
                object_ptr.add(layout.object_properties_offset as usize).cast::<*const qjs::JSValue>().read()
            };
            assert_eq!(unsafe { properties.read().u.int32 }, 9);
            ctx.eval::<(), _>(mutation).unwrap();
            let current_shape = unsafe {
                object_ptr.add(layout.object_shape_offset as usize).cast::<*mut u8>().read()
            };
            let current_generation = unsafe {
                current_shape.add(layout.shape_generation_offset as usize).cast::<u64>().read()
            };
            assert!(current_shape != shape || current_generation != 0x1234_5678_9abc_def0,
                "descriptor mutation retained the guarded identity/token: {mutation}");
            assert_eq!(current_generation, 0, "{mutation}");
        }
    });
}

#[cfg(all(feature = "compiler", feature = "test-support"))]
#[test]
fn property_feedback_tokens_are_stable_then_renewed_after_mutation() {
    use rquickjs::{Context, Object, Runtime};
    use rquickjs_core::qjs;
    use rquickjs_jit::{Jit, JitConfig};
    let runtime = Runtime::new().unwrap();
    let _jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(100_000)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    let mut info: qjs::JSJitABIInfo = unsafe { std::mem::zeroed() };
    info.struct_size = std::mem::size_of_val(&info) as u32;
    assert_eq!(
        unsafe { qjs::JS_GetJitABIInfo(&mut info) },
        qjs::JS_JIT_BACKEND_OK
    );
    context.with(|ctx| {
        let token = |object: &Object| unsafe {
            let ptr = object.as_value().as_raw().u.ptr.cast::<u8>();
            let shape = ptr.add(info.property_layout.object_shape_offset as usize).cast::<*const u8>().read();
            (shape, shape.add(info.property_layout.shape_generation_offset as usize).cast::<u64>().read())
        };
        ctx.eval::<(), _>("globalThis.a={x:1,y:2};globalThis.b={x:3,y:4};function readToken(o){return o.x};readToken(a)").unwrap();
        let a: Object = ctx.globals().get("a").unwrap();
        let b: Object = ctx.globals().get("b").unwrap();
        let first = token(&a);
        assert_ne!(first.1, 0, "interpreter feedback assigns a token");
        assert_eq!(token(&b), first, "canonical shapes share an observation interval");
        ctx.eval::<(), _>("a.x=9;readToken(a)").unwrap();
        assert_eq!(token(&a), first, "value-only stores preserve the observed layout");
        ctx.eval::<(), _>("Object.defineProperty(a,'x',{enumerable:false})").unwrap();
        assert_eq!(token(&a).1, 0, "clone is unobserved");
        assert_eq!(token(&b), first, "copy-on-write leaves the old observed shape valid");
        ctx.eval::<(), _>("readToken(a)").unwrap();
        let second = token(&a);
        assert!(second.1 > first.1, "new observation never reuses the old token");
        ctx.eval::<(), _>("Object.defineProperty(a,'x',{writable:false})").unwrap();
        assert_eq!(token(&a).1, 0, "already-unhashed mutation also invalidates");
        ctx.eval::<(), _>("readToken(a)").unwrap();
        assert!(token(&a).1 > second.1);
    });
}
