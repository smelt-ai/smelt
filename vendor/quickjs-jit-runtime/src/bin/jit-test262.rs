use rquickjs_core::{
    context::EvalOptions,
    loader::{ImportAttributes, Loader, Resolver},
    qjs, ArrayBuffer, Context, Ctx, Error, Function, Module, Object, Runtime, Value,
};
use rquickjs_jit::{
    correctness::{
        classify_features_with_config, compose_test262_program, discover_test262, parse_test262,
        quickjs_config_excludes_path, quickjs_errorfile_contains_path, CaseReport, CaseStatus,
        ExclusionManifest, FeatureDisposition, NativeEvidence, NegativePhase, RunMode, SuiteReport,
        Test262Variant,
    },
    JitConfig, JitRuntime,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    ptr,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct JitTraceEvent {
    pc: u32,
    opcode: u8,
    kind: u8,
    helper_id: u8,
    reserved: u8,
}
unsafe extern "C" {
    fn JS_JitSetExecutionTrace(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        events: *mut JitTraceEvent,
        capacity: u32,
    ) -> i32;
    fn JS_JitGetExecutionTraceLength(
        rt: *mut rquickjs_core::qjs::JSRuntime,
        length: *mut u32,
        overflowed: *mut u32,
    ) -> i32;
}

#[derive(serde::Deserialize)]
struct JitEligibilityManifest {
    cases: Vec<JitEligibleCase>,
}
#[derive(serde::Deserialize)]
struct JitEligibleCase {
    path: String,
    function: String,
    invocation: String,
}

type AfterEvaluate<'a> = &'a dyn Fn(&Context) -> Result<(), String>;

const TEST262_REVISION: &str = "d5e73fc8d2c663554fb72e2380a8c2bc1a318a33";

struct Test262Modules;

fn host_eval_script<'js>(ctx: Ctx<'js>, source: String) -> rquickjs_core::Result<Value<'js>> {
    let mut options = EvalOptions::default();
    options.global = true;
    options.strict = false;
    options.promise = false;
    options.filename = ctx.globals().get("__qjsjitFilename").ok();
    ctx.eval_with_options(source.as_bytes(), options)
}

fn host_print(_: String) {}

fn host_gc(ctx: Ctx<'_>) {
    unsafe {
        qjs::JS_RunGC(qjs::JS_GetRuntime(ctx.as_raw().as_ptr()));
    }
}

impl Resolver for Test262Modules {
    fn resolve<'js>(
        &mut self,
        _: &Ctx<'js>,
        base: &str,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs_core::Result<String> {
        let path = if name.starts_with('.') {
            Path::new(base).parent().unwrap_or(Path::new("")).join(name)
        } else {
            PathBuf::from(name)
        };
        // QuickJS keys modules by the resolver string. Canonicalize existing
        // files so `file.js` and `./file.js` share one module record; without
        // this, self-import tests are evaluated twice and the second raw load
        // lacks the harness globals installed in the root module.
        Ok(path
            .canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned())
    }
}
impl Loader for Test262Modules {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs_core::Result<Module<'js>> {
        let source = fs::read(name).map_err(|_| Error::new_loading(name))?;
        Module::declare(ctx.clone(), name, source)
    }
}

struct Options {
    root: PathBuf,
    output: PathBuf,
    mode: RunMode,
    limit: Option<usize>,
    filter: Option<String>,
    shard_index: usize,
    shard_count: usize,
    timeout: Duration,
}

fn options() -> Result<Options, String> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("jit crate has workspace parent");
    let mut root = workspace_root.join("sys/quickjs/test262");
    let mut output = PathBuf::from("target/jit-test262/report.json");
    let mut mode = RunMode::Interpreter;
    let mut limit = None;
    let mut filter = None;
    let mut shard_index = 0;
    let mut shard_count = 1;
    let mut timeout = Duration::from_secs(10);
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = |args: &mut std::iter::Skip<std::env::Args>, name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--root" => root = value(&mut args, "--root")?.into(),
            "--output" => output = value(&mut args, "--output")?.into(),
            "--mode" => mode = RunMode::from_str(&value(&mut args, "--mode")?)?,
            "--limit" => {
                limit = Some(
                    value(&mut args, "--limit")?
                        .parse()
                        .map_err(|_| "invalid --limit")?,
                )
            }
            "--filter" => filter = Some(value(&mut args, "--filter")?),
            "--shard-index" => {
                shard_index = value(&mut args, "--shard-index")?
                    .parse()
                    .map_err(|_| "invalid --shard-index")?
            }
            "--shard-count" => {
                shard_count = value(&mut args, "--shard-count")?
                    .parse()
                    .map_err(|_| "invalid --shard-count")?
            }
            "--timeout-ms" => {
                timeout = Duration::from_millis(
                    value(&mut args, "--timeout-ms")?
                        .parse()
                        .map_err(|_| "invalid --timeout-ms")?,
                )
            }
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    if shard_count == 0 || shard_index >= shard_count {
        return Err("shard index must be less than nonzero shard count".into());
    }
    Ok(Options {
        root,
        output,
        mode,
        limit,
        filter,
        shard_index,
        shard_count,
        timeout,
    })
}

fn harness(root: &Path, includes: &[String], asynchronous: bool) -> Result<String, String> {
    let mut source = String::new();
    for include in ["sta.js", "assert.js"]
        .into_iter()
        .chain(includes.iter().map(String::as_str))
    {
        let path = root.join("harness").join(include);
        source.push_str(
            &fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?,
        );
        source.push('\n');
    }
    if asynchronous {
        source.push_str("globalThis.__qjsjitDone=false;globalThis.__qjsjitAsyncError=null;globalThis.$DONE=e=>{__qjsjitAsyncError=e==null?null:String(e&&e.stack||e);__qjsjitDone=true};\n");
    }
    Ok(source)
}

fn execute(
    runtime: &Runtime,
    root: &Path,
    case: &rquickjs_jit::correctness::Test262Case,
    variant: Test262Variant,
    timeout: Duration,
    after_evaluate: Option<AfterEvaluate<'_>>,
) -> Result<(), String> {
    runtime.set_loader(Test262Modules, Test262Modules);
    let deadline = Instant::now() + timeout;
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&interrupted);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let expired = Instant::now() >= deadline;
        if expired {
            flag.store(true, Ordering::SeqCst);
        }
        expired
    })));
    let asynchronous = matches!(
        variant,
        Test262Variant::AsyncScript
            | Test262Variant::StrictAsyncScript
            | Test262Variant::ModuleAsync
            | Test262Variant::StrictModuleAsync
    );
    let harness = if matches!(
        variant,
        Test262Variant::RawScript | Test262Variant::RawModule
    ) {
        String::new()
    } else {
        harness(root, case.includes(), asynchronous)?
    };
    let program = compose_test262_program(case, variant, &harness);
    let module = matches!(
        variant,
        Test262Variant::RawModule
            | Test262Variant::Module
            | Test262Variant::StrictModule
            | Test262Variant::ModuleAsync
            | Test262Variant::StrictModuleAsync
    );
    let filename_path = root.join(case.path());
    let filename = filename_path
        .canonicalize()
        .unwrap_or(filename_path)
        .to_string_lossy()
        .into_owned();
    let context = Context::full(runtime).map_err(|error| format!("context: {error:?}"))?;
    let strict = matches!(
        variant,
        Test262Variant::StrictScript
            | Test262Variant::StrictAsyncScript
            | Test262Variant::StrictModule
            | Test262Variant::StrictModuleAsync
    );
    let result: Result<(), (NegativePhase, String)> = context.with(|ctx| {
        let detail = |error: Error| {
            let caught = ctx.catch();
            caught
                .into_object()
                .and_then(|object| {
                    let name = object.get::<_, String>("name").ok().or_else(|| {
                        object
                            .get::<_, Object<'_>>("constructor")
                            .ok()?
                            .get::<_, String>("name")
                            .ok()
                    })?;
                    let message = object.get::<_, String>("message").ok().unwrap_or_default();
                    Some(format!("{name}: {message}"))
                })
                .unwrap_or_else(|| format!("{error:?}"))
        };
        // Test262's detachArrayBuffer.js harness delegates to the host-defined
        // `$262.detachArrayBuffer`. QuickJS exposes the operation directly, so
        // install the smallest host object needed by applicable conformance
        // cases instead of excluding those cases from the corpus.
        let host =
            Object::new(ctx.clone()).map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        let detach = Function::new(ctx.clone(), |mut buffer: ArrayBuffer<'_>| {
            buffer.detach();
        })
        .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        host.set("detachArrayBuffer", detach)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        let eval_script = Function::new(ctx.clone(), host_eval_script)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        host.set("evalScript", eval_script)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        host.set("global", ctx.globals())
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        let gc = Function::new(ctx.clone(), host_gc)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        host.set("gc", gc)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        ctx.globals()
            .set("$262", host)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        ctx.globals()
            .set("__qjsjitFilename", filename.clone())
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        let print = Function::new(ctx.clone(), host_print)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        ctx.globals()
            .set("print", print)
            .map_err(|error| (NegativePhase::Runtime, detail(error)))?;
        if case
            .negative()
            .is_some_and(|negative| negative.phase() == NegativePhase::Parse)
        {
            let parsed = if module {
                Module::declare(ctx.clone(), filename.as_str(), program.as_bytes()).map(|_| ())
            } else {
                ctx.compile(program.as_bytes(), filename.as_bytes(), strict)
            };
            return parsed.map_err(|error| (NegativePhase::Parse, detail(error)));
        }
        if module {
            let declared = Module::declare(ctx.clone(), filename.as_str(), program.as_bytes())
                // QuickJS resolves and parses imported modules while declaring
                // the root module. The root was already handled above for an
                // expected parse-negative case, so failures reached here are
                // module-resolution failures (including invalid dependencies).
                .map_err(|error| (NegativePhase::Resolution, detail(error)))?;
            let (_, promise) = declared
                .eval()
                .map_err(|error| (NegativePhase::Resolution, detail(error)))?;
            promise
                .finish::<()>()
                .map_err(|error| (NegativePhase::Runtime, detail(error)))
        } else {
            let mut options = EvalOptions::default();
            options.global = true;
            options.strict = strict;
            options.promise = false;
            options.filename = Some(filename.clone());
            ctx.eval_with_options::<(), _>(program.as_bytes(), options)
                .map_err(|error| (NegativePhase::Runtime, detail(error)))
        }
    });
    let mut observed_error = result.err();
    while runtime.is_job_pending() && Instant::now() < deadline {
        if let Err(error) = runtime.execute_pending_job() {
            observed_error = Some((NegativePhase::Runtime, format!("pending job: {error:?}")));
            break;
        }
    }
    if observed_error.is_none() {
        if let Some(callback) = after_evaluate {
            callback(&context)?;
        }
    }
    if interrupted.load(Ordering::SeqCst) || Instant::now() >= deadline {
        return Err(format!("timeout after {} ms", timeout.as_millis()));
    }
    if asynchronous {
        let (done, error) = context.with(|ctx| {
            let globals = ctx.globals();
            let done = globals
                .get::<_, bool>("__qjsjitDone")
                .map_err(|error| format!("async completion flag: {error:?}"))?;
            let error = globals
                .get::<_, Option<String>>("__qjsjitAsyncError")
                .map_err(|error| format!("async completion error: {error:?}"))?;
            Ok::<_, String>((done, error))
        })?;
        if !done || error.is_some() {
            return Err(format!(
                "async $DONE did not complete successfully: done={done}, error={error:?}"
            ));
        }
    }
    match case.negative() {
        None => observed_error.map_or(Ok(()), |(phase, error)| Err(format!("{phase:?}: {error}"))),
        Some(negative) => match observed_error {
            Some((phase, error))
                if phase == negative.phase() && error.contains(negative.error_type()) =>
            {
                Ok(())
            }
            Some((phase, error)) => Err(format!(
                "expected {:?} {}, got {phase:?} {error}",
                negative.phase(),
                negative.error_type()
            )),
            None => Err(format!(
                "expected {} {:?} failure",
                negative.error_type(),
                negative.phase()
            )),
        },
    }
}

fn run(options: Options) -> Result<SuiteReport, String> {
    let test_root = options.root.join("test");
    let mut paths = discover_test262(&test_root).map_err(|error| error.to_string())?;
    paths.retain(|path| {
        !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("_FIXTURE"))
    });
    let discovered_files = paths.len();
    if let Some(filter) = &options.filter {
        paths.retain(|path| path.to_string_lossy().contains(filter));
    }
    paths = paths
        .into_iter()
        .enumerate()
        .filter_map(|(index, path)| {
            (index % options.shard_count == options.shard_index).then_some(path)
        })
        .collect();
    if let Some(limit) = options.limit {
        paths.truncate(limit);
    }
    if paths.is_empty() {
        return Err("selected shard contains zero Test262 cases".into());
    }
    let mut reports = Vec::new();
    let feature_registry = fs::read_to_string(options.root.join("features.txt"))
        .map_err(|error| format!("features.txt: {error}"))?;
    let quickjs_config = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("sys/quickjs/test262.conf"),
    )
    .map_err(|error| format!("test262.conf: {error}"))?;
    let quickjs_errors = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("sys/quickjs/test262_errors.txt"),
    )
    .map_err(|error| format!("test262_errors.txt: {error}"))?;
    let exclusions: ExclusionManifest =
        serde_json::from_str(include_str!("../../tests/fixtures/test262-exclusions.json"))
            .map_err(|error| format!("invalid exclusion manifest: {error}"))?;
    exclusions.validate("2026-08-30")?;
    let jit_eligibility: JitEligibilityManifest = serde_json::from_str(include_str!(
        "../../tests/fixtures/test262-jit-eligible.json"
    ))
    .map_err(|error| format!("invalid JIT eligibility manifest: {error}"))?;
    for path in paths {
        let relative = path
            .strip_prefix(&options.root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let source =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let case = match parse_test262(&relative, &source) {
            Ok(case) => case,
            Err(error) => {
                reports.push(failure(
                    &relative,
                    Test262Variant::RawScript,
                    error.to_string(),
                ));
                continue;
            }
        };
        let feature_disposition =
            classify_features_with_config(case.features(), &feature_registry, &quickjs_config)?;
        for &variant in case.variants() {
            let jit_case = jit_eligibility
                .cases
                .iter()
                .find(|entry| entry.path == relative);
            if matches!(options.mode, RunMode::ForceTier1 | RunMode::ForceTier2)
                && jit_case.is_none()
            {
                reports.push(CaseReport {
                    path: relative.clone(),
                    variant,
                    status: CaseStatus::Skip,
                    duration_ms: 0,
                    negative_phase: case.negative().map(|n| n.phase()),
                    negative_type: case.negative().map(|n| n.error_type().to_owned()),
                    skip_reason: Some(
                        "tier-ineligible: not listed in test262-jit-eligible.json".into(),
                    ),
                    error: None,
                    native: NativeEvidence {
                        native_entries: 0,
                        tier2_entries: 0,
                        native_exits: 0,
                        unexpected_fallbacks: 0,
                        opcode_ids: vec![],
                        helper_ids: vec![],
                    },
                });
                continue;
            }
            let path_exclusion = exclusions
                .path_reason(&relative)
                .or_else(|| {
                    quickjs_config_excludes_path(&quickjs_config, &relative)
                        .then(|| "excluded by the pinned QuickJS test262.conf baseline".to_owned())
                })
                .or_else(|| {
                    quickjs_errorfile_contains_path(&quickjs_errors, &relative).then(|| {
                        "known failure in pinned QuickJS test262_errors.txt baseline".to_owned()
                    })
                });
            if let Some(reason) = path_exclusion {
                reports.push(CaseReport {
                    path: relative.clone(),
                    variant,
                    status: CaseStatus::Skip,
                    duration_ms: 0,
                    negative_phase: case.negative().map(|n| n.phase()),
                    negative_type: case.negative().map(|n| n.error_type().to_owned()),
                    skip_reason: Some(reason),
                    error: None,
                    native: NativeEvidence {
                        native_entries: 0,
                        tier2_entries: 0,
                        native_exits: 0,
                        unexpected_fallbacks: 0,
                        opcode_ids: vec![],
                        helper_ids: vec![],
                    },
                });
                continue;
            }
            if let FeatureDisposition::Unsupported(features) = &feature_disposition {
                let reason = exclusions
                    .reason_for(&relative, features)
                    .or_else(|| exclusions.reason_for(&relative, &["@quickjs-config-skip".into()]))
                    .ok_or_else(|| {
                        format!(
                            "unsupported features lack checked exclusion: {}",
                            features.join(", ")
                        )
                    })?;
                reports.push(CaseReport {
                    path: relative.clone(),
                    variant,
                    status: CaseStatus::Skip,
                    duration_ms: 0,
                    negative_phase: case.negative().map(|n| n.phase()),
                    negative_type: case.negative().map(|n| n.error_type().to_owned()),
                    skip_reason: Some(reason),
                    error: None,
                    native: NativeEvidence {
                        native_entries: 0,
                        tier2_entries: 0,
                        native_exits: 0,
                        unexpected_fallbacks: 0,
                        opcode_ids: vec![],
                        helper_ids: vec![],
                    },
                });
                continue;
            }
            let started = Instant::now();
            let (result, evidence) = match options.mode {
                RunMode::Interpreter => {
                    let runtime = Runtime::new().map_err(|error| format!("runtime: {error:?}"))?;
                    (
                        execute(
                            &runtime,
                            &options.root,
                            &case,
                            variant,
                            options.timeout,
                            None,
                        ),
                        NativeEvidence {
                            native_entries: 0,
                            tier2_entries: 0,
                            native_exits: 0,
                            unexpected_fallbacks: 0,
                            opcode_ids: vec![],
                            helper_ids: vec![],
                        },
                    )
                }
                _ => {
                    let trace_evidence =
                        Arc::new(Mutex::new((Vec::<u16>::new(), Vec::<u16>::new())));
                    let config = if options.mode == RunMode::Automatic {
                        JitConfig::default()
                    } else {
                        let builder = JitConfig::builder().call_threshold(1).loop_threshold(1);
                        #[cfg(feature = "test-support")]
                        let builder =
                            builder.force_optimized_for_test(options.mode == RunMode::ForceTier2);
                        builder.build().map_err(|e| e.to_string())?
                    };
                    let runtime = JitRuntime::builder()
                        .config(config)
                        .build()
                        .map_err(|error| format!("JIT runtime: {error:?}"))?;
                    let forced = jit_case.map(|eligible| {
                        let function = eligible.function.clone();
                        let invocation = eligible.invocation.clone();
                        let mode = options.mode;
                        let timeout = options.timeout;
                        let runtime_ref = &runtime;
                        let trace_evidence = Arc::clone(&trace_evidence);
                        move |context: &Context| -> Result<(), String> {
                            let rt = context.with(|ctx| unsafe {
                                rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr())
                            });
                            let mut trace = vec![JitTraceEvent::default(); 65_536];
                            let warm = format!(
                                "for(let __jit_i=0;__jit_i<256;__jit_i++){{{invocation};}}"
                            );
                            // Compiler readiness depends on elapsed time, not
                            // interpreter replay speed (especially with coverage
                            // or sanitizer instrumentation on the worker).
                            let replay_deadline = Instant::now() + timeout;
                            while Instant::now() < replay_deadline {
                                // Re-arm the trace for every replay round. The
                                // evidence only needs one round in which native
                                // code executed; accumulating every warm-up
                                // round overflows the bounded buffer once the
                                // harness's own assertion helpers run natively.
                                if unsafe {
                                    JS_JitSetExecutionTrace(
                                        rt,
                                        trace.as_mut_ptr(),
                                        trace.len() as u32,
                                    )
                                } != 0
                                {
                                    return Err("failed to arm native execution trace".into());
                                }
                                context
                                    .with(|ctx| ctx.eval::<(), _>(warm.as_str()))
                                    .map_err(|error| {
                                        format!("forced {function} replay: {error:?}")
                                    })?;
                                runtime_ref.jit().poll();
                                let metrics = runtime_ref.metrics();
                                if metrics.native_entries > 0
                                    && (mode != RunMode::ForceTier2 || metrics.tier2_entries > 0)
                                {
                                    let mut length = 0;
                                    let mut overflowed = 0;
                                    if unsafe {
                                        JS_JitGetExecutionTraceLength(
                                            rt,
                                            &mut length,
                                            &mut overflowed,
                                        )
                                    } != 0
                                        || overflowed != 0
                                    {
                                        return Err(
                                            "native execution trace failed or overflowed".into()
                                        );
                                    }
                                    trace.truncate(length as usize);
                                    unsafe { JS_JitSetExecutionTrace(rt, ptr::null_mut(), 0) };
                                    let mut evidence =
                                        trace_evidence.lock().unwrap_or_else(|p| p.into_inner());
                                    evidence.0 = trace
                                        .iter()
                                        .filter(|e| e.kind == 0)
                                        .map(|e| u16::from(e.opcode))
                                        .collect();
                                    evidence.1 = trace
                                        .iter()
                                        .filter(|e| e.kind == 1)
                                        .map(|e| u16::from(e.helper_id))
                                        .collect();
                                    evidence.0.sort_unstable();
                                    evidence.0.dedup();
                                    evidence.1.sort_unstable();
                                    evidence.1.dedup();
                                    return Ok(());
                                }
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(format!("forced {function} did not enter requested tier"))
                        }
                    });
                    let result = execute(
                        &runtime,
                        &options.root,
                        &case,
                        variant,
                        options.timeout,
                        forced
                            .as_ref()
                            .map(|f| f as &dyn Fn(&Context) -> Result<(), String>),
                    );
                    runtime.jit().poll();
                    let metrics = runtime.metrics();
                    let trace = trace_evidence.lock().unwrap_or_else(|p| p.into_inner());
                    (
                        result,
                        NativeEvidence {
                            native_entries: metrics.native_entries,
                            tier2_entries: metrics.tier2_entries,
                            native_exits: metrics.native_exits,
                            unexpected_fallbacks: metrics.native_fallbacks,
                            opcode_ids: trace.0.clone(),
                            helper_ids: trace.1.clone(),
                        },
                    )
                }
            };
            reports.push(CaseReport {
                path: relative.clone(),
                variant,
                status: if result.is_ok() {
                    CaseStatus::Pass
                } else {
                    CaseStatus::Fail
                },
                duration_ms: started.elapsed().as_millis() as u64,
                negative_phase: case.negative().map(|n| n.phase()),
                negative_type: case.negative().map(|n| n.error_type().to_owned()),
                skip_reason: None,
                error: result.err(),
                native: evidence,
            });
        }
    }
    let report = SuiteReport::new("test262-rust-host", TEST262_REVISION, options.mode, reports)
        .with_discovery(discovered_files, options.shard_index, options.shard_count);
    report.validate()?;
    Ok(report)
}

fn failure(path: &str, variant: Test262Variant, error: String) -> CaseReport {
    CaseReport {
        path: path.into(),
        variant,
        status: CaseStatus::Fail,
        duration_ms: 0,
        negative_phase: None,
        negative_type: None,
        skip_reason: None,
        error: Some(error),
        native: NativeEvidence {
            native_entries: 0,
            tier2_entries: 0,
            native_exits: 0,
            unexpected_fallbacks: 0,
            opcode_ids: vec![],
            helper_ids: vec![],
        },
    }
}

fn main() -> ExitCode {
    let options = match options() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("jit-test262: {e}");
            return ExitCode::FAILURE;
        }
    };
    let output = options.output.clone();
    match run(options) {
        Ok(report) => {
            if let Some(parent) = output.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    eprintln!("{}: {e}", parent.display());
                    return ExitCode::FAILURE;
                }
            }
            if let Err(e) = fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()) {
                eprintln!("{}: {e}", output.display());
                return ExitCode::FAILURE;
            }
            let failed = report
                .cases
                .iter()
                .filter(|c| matches!(c.status, CaseStatus::Fail))
                .count();
            eprintln!(
                "{}: {} variants, {failed} failed",
                output.display(),
                report.cases.len()
            );
            if failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("jit-test262: {error}");
            ExitCode::FAILURE
        }
    }
}
