use rquickjs_core::{Context, Runtime};
use rquickjs_jit::correctness::{
    canonical_observation_source, canonical_observer_prelude, classify_features,
    classify_features_against, classify_features_with_config, compose_test262_program,
    discover_test262, parse_test262, quickjs_config_excludes_path, quickjs_errorfile_contains_path,
    Exclusion, ExclusionManifest, FeatureDisposition, NegativePhase, RunMode, StructuredProgram,
    SuiteReport, Test262Variant,
};
use std::fs;
use std::path::Path;

const FULL_METADATA: &str = r#"
/*---
description: metadata is not optional
flags: [onlyStrict, async, module]
includes: [assert.js, sta.js]
features: [BigInt, Symbol]
negative:
  phase: runtime
  type: TypeError
---*/
throw new TypeError();
"#;

#[test]
fn parses_real_test262_yaml_and_derives_an_exact_variant() {
    let test = parse_test262("language/example.js", FULL_METADATA).unwrap();
    assert_eq!(test.includes(), ["assert.js", "sta.js"]);
    assert_eq!(test.features(), ["BigInt", "Symbol"]);
    assert_eq!(test.negative().unwrap().phase(), NegativePhase::Runtime);
    assert_eq!(test.negative().unwrap().error_type(), "TypeError");
    assert_eq!(test.variants(), [Test262Variant::StrictModuleAsync]);
    assert_eq!(test.body(), "throw new TypeError();\n");
    assert_eq!(test.source(), FULL_METADATA);
    assert_eq!(
        &test.source()[test.body_range()],
        "throw new TypeError();\n"
    );
    assert!(test.source()[test.metadata_range()].starts_with("/*---"));
}

#[test]
fn parser_preserves_raw_lexemes_and_only_removes_the_frontmatter_from_body() {
    let source =
        "// license\r\n/*---\r\nflags: [raw]\r\n---*/\r\n'\\u0061'; // keep CRLF and escape\r\n";
    let test = parse_test262("raw.js", source).unwrap();
    assert_eq!(test.source().as_bytes(), source.as_bytes());
    assert_eq!(test.body(), "'\\u0061'; // keep CRLF and escape\r\n");
    assert_eq!(&test.source()[test.body_range()], test.body());
}

#[test]
fn report_refuses_zero_executed_variants_and_modes_are_strict() {
    assert_eq!(
        "force-tier2".parse::<RunMode>().unwrap(),
        RunMode::ForceTier2
    );
    assert!("tier1-ish".parse::<RunMode>().is_err());
    let report = SuiteReport::new("test262", "deadbeef", RunMode::Automatic, vec![]);
    assert!(report.validate().unwrap_err().contains("zero cases"));
}

#[test]
fn forced_reports_allow_explicitly_ineligible_skips_but_never_native_less_passes() {
    use rquickjs_jit::correctness::{CaseReport, CaseStatus, NativeEvidence};
    let skip = CaseReport {
        path: "unsupported.js".into(),
        variant: Test262Variant::RawScript,
        status: CaseStatus::Skip,
        duration_ms: 0,
        negative_phase: None,
        negative_type: None,
        skip_reason: Some("tier1-ineligible: eval opcode".into()),
        error: None,
        native: NativeEvidence {
            native_entries: 0,
            tier2_entries: 0,
            native_exits: 0,
            unexpected_fallbacks: 0,
            opcode_ids: vec![],
            helper_ids: vec![],
        },
    };
    SuiteReport::new("test262", "deadbeef", RunMode::ForceTier1, vec![skip])
        .validate()
        .unwrap();
}

#[test]
fn shard_merge_rejects_missing_duplicate_and_zero_case_shards() {
    use rquickjs_jit::correctness::{CaseReport, CaseStatus, NativeEvidence};
    let case = |path: &str| CaseReport {
        path: path.into(),
        variant: Test262Variant::RawScript,
        status: CaseStatus::Pass,
        duration_ms: 0,
        negative_phase: None,
        negative_type: None,
        skip_reason: None,
        error: None,
        native: NativeEvidence {
            native_entries: 0,
            tier2_entries: 0,
            native_exits: 0,
            unexpected_fallbacks: 0,
            opcode_ids: vec![],
            helper_ids: vec![],
        },
    };
    let shard = |index, cases| {
        SuiteReport::new("test262", "rev", RunMode::Automatic, cases).with_discovery(2, index, 2)
    };
    assert!(SuiteReport::merge_shards(vec![shard(0, vec![case("a")])])
        .unwrap_err()
        .contains("missing shard"));
    assert!(
        SuiteReport::merge_shards(vec![shard(0, vec![case("a")]), shard(0, vec![case("b")])])
            .unwrap_err()
            .contains("duplicate shard")
    );
    assert!(
        SuiteReport::merge_shards(vec![shard(0, vec![]), shard(1, vec![case("b")])])
            .unwrap_err()
            .contains("zero cases")
    );
    let merged =
        SuiteReport::merge_shards(vec![shard(1, vec![case("b")]), shard(0, vec![case("a")])])
            .unwrap();
    assert_eq!(
        merged
            .cases
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn structured_programs_are_seeded_terminating_and_shrinkable() {
    let program = StructuredProgram::generate(0x1234_5678, 24);
    assert_eq!(program, StructuredProgram::generate(0x1234_5678, 24));
    assert!(program.source().contains("for(let i=0;i<"));
    assert!(program.source().contains("Proxy"));
    assert!(program.source().contains("events.join(',')"));
    assert!(!program.source().contains("return {total"));
    let shrunk = program.shrink();
    assert!(shrunk.fuel() < program.fuel());
    assert_eq!(shrunk.seed(), program.seed());
}

#[test]
fn every_fuzz_seed_has_current_version_and_opcode_fingerprint() {
    const PREFIX: &[u8] = b"QJSJFZ01:05d5c0867521c077:";
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus");
    let mut pending = vec![corpus];
    let mut checked = 0;

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) != Some("README.md") {
                let seed = std::fs::read(&path).unwrap();
                assert!(
                    seed.starts_with(PREFIX),
                    "{} lacks the current fuzz format and opcode fingerprint",
                    path.display()
                );
                assert!(
                    seed.len() > PREFIX.len(),
                    "{} has no seed payload",
                    path.display()
                );
                checked += 1;
            }
        }
    }

    assert!(checked > 0, "the fuzz corpus must contain fixed seeds");
}

#[test]
fn ordinary_scripts_run_both_strict_variants_but_raw_runs_once() {
    let ordinary = parse_test262("ordinary.js", "/*---\n---*/\n1 + 1;\n").unwrap();
    assert_eq!(
        ordinary.variants(),
        [Test262Variant::SloppyScript, Test262Variant::StrictScript]
    );
    let raw = parse_test262("raw.js", "/*---\nflags: [raw]\n---*/\n1 + 1;\n").unwrap();
    assert_eq!(raw.variants(), [Test262Variant::RawScript]);

    let raw_module = parse_test262(
        "raw-module.js",
        "/*---\nflags: [raw, module]\n---*/\nexport {};",
    )
    .unwrap();
    assert_eq!(raw_module.variants(), [Test262Variant::RawModule]);
    assert_eq!(
        compose_test262_program(&raw_module, Test262Variant::RawModule, "HARNESS"),
        raw_module.source()
    );
}

#[test]
fn raw_programs_receive_exact_body_without_harness_or_strict_directive() {
    let raw = parse_test262(
        "raw.js",
        "/*---\nflags: [raw]\nincludes: [assert.js]\n---*/\nlet x=1;\n",
    )
    .unwrap();
    assert_eq!(
        compose_test262_program(&raw, Test262Variant::RawScript, "HARNESS"),
        raw.source()
    );
    let strict =
        parse_test262("strict.js", "/*---\nflags: [onlyStrict]\n---*/\nlet x=1;\n").unwrap();
    assert_eq!(
        compose_test262_program(&strict, Test262Variant::StrictScript, "HARNESS"),
        "HARNESS\n'use strict';\nlet x=1;\n"
    );
}

#[test]
fn annex_b_raw_source_executes_text_before_frontmatter() {
    // Keep this regression self-contained: CI does not initialize the optional
    // upstream Test262 checkout used by the exhaustive conformance runner.
    let pinned = "/* a comment */ /*another comment*/--> a comment\n\
/*---\nflags: [raw]\nnegative:\n  phase: runtime\n  type: EvalError\n---*/\n\
throw new EvalError(\"This is not in a comment\");\n";
    assert!(pinned.starts_with("/* a comment */ /*another comment*/--> a comment"));
    let case = parse_test262("annex-b-raw.js", pinned).unwrap();
    let program = compose_test262_program(&case, Test262Variant::RawScript, "throw 'harness'");
    assert_eq!(program.as_bytes(), pinned.as_bytes());
    let runtime = Runtime::new().unwrap();
    Context::full(&runtime).unwrap().with(|ctx| {
        let error = ctx.eval::<(), _>(program.as_str()).unwrap_err();
        assert!(error.to_string().contains("Exception generated by QuickJS"));
        let exception = ctx.catch();
        let exception = exception.as_object().unwrap();
        assert_eq!(exception.get::<_, String>("name").unwrap(), "EvalError");
        assert_eq!(
            exception.get::<_, String>("message").unwrap(),
            "This is not in a comment"
        );
    });
}

#[test]
fn script_compile_only_detects_parse_errors_without_running_side_effects() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.compile("globalThis.__ran = true", "parse-only.js", false)
            .unwrap();
        assert!(ctx
            .eval::<bool, _>("globalThis.__ran === undefined")
            .unwrap());
        assert!(ctx.compile("let = ;", "syntax.js", false).is_err());
    });
}

#[test]
fn malformed_metadata_and_unknown_flags_fail_closed() {
    assert!(parse_test262("bad.js", "1 + 1")
        .unwrap_err()
        .to_string()
        .contains("frontmatter"));
    assert!(
        parse_test262("bad.js", "/*---\nflags: [inventedFlag]\n---*/\n1 + 1;\n")
            .unwrap_err()
            .to_string()
            .contains("unknown flag")
    );
}

#[test]
fn exclusion_manifest_requires_accountability_and_rejects_expired_entries() {
    let manifest = ExclusionManifest::new(vec![Exclusion {
        pattern: "built-ins/Atomics/**".into(),
        reason: "agent API is not available in the Rust host".into(),
        owner: "jit-runtime".into(),
        expires: "2099-01-01".into(),
        features: vec!["Atomics".into()],
    }]);
    manifest.validate("2026-08-30").unwrap();

    let expired = ExclusionManifest::new(vec![Exclusion {
        pattern: "**".into(),
        reason: "temporary".into(),
        owner: "nobody".into(),
        expires: "2020-01-01".into(),
        features: vec![],
    }]);
    assert!(expired
        .validate("2026-08-30")
        .unwrap_err()
        .contains("expired"));
}

#[test]
fn suite_discovery_fails_instead_of_reporting_zero_cases() {
    let root = std::env::temp_dir().join(format!("qjsjit-empty-suite-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let error = discover_test262(&root).unwrap_err();
    assert!(error.to_string().contains("zero Test262 cases"));
    fs::remove_dir(root).unwrap();
}

#[test]
fn unknown_features_are_errors_and_explicit_unsupported_features_skip() {
    assert_eq!(
        classify_features(&["BigInt".into()]).unwrap(),
        FeatureDisposition::Supported
    );
    assert!(classify_features(&["future-invention".into()])
        .unwrap_err()
        .contains("unknown Test262 feature"));
    assert_eq!(
        classify_features(&["agent".into()]).unwrap(),
        FeatureDisposition::Unsupported(vec!["agent".into()])
    );
}

#[test]
fn pinned_registry_distinguishes_known_feature_from_typo() {
    let registry = "Symbol.iterator\nIsHTMLDDA\n";
    assert_eq!(
        classify_features_against(&["Symbol.iterator".into()], registry).unwrap(),
        FeatureDisposition::Supported
    );
    assert_eq!(
        classify_features_against(&["IsHTMLDDA".into()], registry).unwrap(),
        FeatureDisposition::Unsupported(vec!["IsHTMLDDA".into()])
    );
    assert!(classify_features_against(&["Symbol.iteratr".into()], registry).is_err());
}

#[test]
fn pinned_quickjs_config_skips_only_explicit_feature_assignments() {
    let registry = "legacy-regexp\nReflect\ncross-realm\n";
    let config = "[features]\nlegacy-regexp=skip\nReflect\ncross-realm\n";
    assert_eq!(
        classify_features_with_config(
            &["legacy-regexp".into(), "Reflect".into()],
            registry,
            config
        )
        .unwrap(),
        FeatureDisposition::Unsupported(vec!["legacy-regexp".into()])
    );
    assert_eq!(
        classify_features_with_config(&["Reflect".into()], registry, config).unwrap(),
        FeatureDisposition::Supported
    );
}

#[test]
fn pinned_quickjs_path_exclusions_honor_exact_prefix_and_reinclude_rules() {
    let config = "[features]\nfoo=skip\n[exclude]\ntest262/test/exact.js\ntest262/test/staging/\n!test262/test/staging/kept/\n[tests]\n";
    assert!(quickjs_config_excludes_path(config, "test/exact.js"));
    assert!(quickjs_config_excludes_path(
        config,
        "test/staging/dropped.js"
    ));
    assert!(!quickjs_config_excludes_path(
        config,
        "test/staging/kept/case.js"
    ));
    assert!(!quickjs_config_excludes_path(config, "test/exact.js-extra"));
}

#[test]
fn pinned_quickjs_errorfile_matches_only_the_exact_test_path() {
    let errors = "test262/test/a.js:12: TypeError: expected\n\
                  test262/test/a.js:12: strict mode: TypeError: expected\n";
    assert!(quickjs_errorfile_contains_path(errors, "test/a.js"));
    assert!(!quickjs_errorfile_contains_path(errors, "test/a.js-extra"));
    assert!(!quickjs_errorfile_contains_path(errors, "test/b.js"));
}

#[test]
fn canonical_observation_preserves_values_json_loses() {
    let source =
        canonical_observation_source("({nan:NaN, negzero:-0, big:2n, hole:[,1], sym:Symbol('x')})");
    let prelude = canonical_observer_prelude();
    assert!(!source.contains("Object.getOwnPropertyDescriptors(value)"));
    assert!(!source.contains("instanceof"));
    assert!(!source.contains("Object.prototype.toString.call(value)"));
    assert!(prelude.contains("const $numberIsNaN = Number.isNaN"));
    assert!(prelude.contains("const $objectIs = Object.is"));
    assert!(prelude.contains("opaque:true"));
    assert!(prelude.contains("coverage:'primitive-only'"));
}

#[test]
fn canonical_primitive_observer_survives_intrinsic_poisoning_without_events() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(canonical_observer_prelude()).unwrap();
        let clean: String = ctx.eval(canonical_observation_source("NaN")).unwrap();
        let poisoned: String = ctx
            .eval(canonical_observation_source(
                r#"(()=>{globalThis.__events=[];
                Number.isNaN=()=>{__events.push('nan');throw 1};
                Object.is=()=>{__events.push('is');throw 1};
                Symbol.keyFor=()=>{__events.push('key');throw 1};
                String=()=>{__events.push('string');throw 1};
                JSON.stringify=()=>{__events.push('json');throw 1};
                return NaN})()"#,
            ))
            .unwrap();
        assert_eq!(poisoned, clean);
        // Do not use the now-poisoned JSON.stringify to inspect the log.
        assert_eq!(ctx.eval::<i32, _>("__events.length").unwrap(), 0);
    });
}

#[test]
fn trusted_observer_installation_fails_closed_on_a_preexisting_fake_handle() {
    let runtime = Runtime::new().unwrap();
    Context::full(&runtime).unwrap().with(|ctx| {
        ctx.eval::<(), _>("Object.defineProperty(globalThis,'__rquickjsTrustedObserve',{value:()=>\"fake\",configurable:false})").unwrap();
        assert!(ctx.eval::<(), _>(canonical_observer_prelude()).is_err());
        assert_eq!(ctx.eval::<String, _>("__rquickjsTrustedObserve()").unwrap(), "fake");
    });
}

#[test]
fn canonical_observation_does_not_trigger_proxy_reflection_or_reorder_events() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(canonical_observer_prelude()).unwrap();
        let expression = r#"(()=>{globalThis.__events=[];return new Proxy({}, {
            ownKeys(){__events.push('ownKeys');return []},
            getOwnPropertyDescriptor(){__events.push('descriptor');return undefined},
            getPrototypeOf(){__events.push('prototype');return null},
            get(){__events.push('get');return undefined}
        })})()"#;
        let observed: String = ctx.eval(canonical_observation_source(expression)).unwrap();
        assert!(observed.contains("\"opaque\":true"));
        assert_eq!(
            ctx.eval::<String, _>("JSON.stringify(__events)").unwrap(),
            "[]"
        );
    });
}
