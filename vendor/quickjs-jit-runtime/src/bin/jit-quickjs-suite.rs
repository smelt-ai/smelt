use rquickjs_core::{Context, Runtime};
use rquickjs_jit::{
    correctness::{CaseReport, CaseStatus, NativeEvidence, RunMode, SuiteReport, Test262Variant},
    JitRuntime,
};
use std::{env, fs, path::PathBuf, process::ExitCode, str::FromStr, time::Instant};

fn evidence() -> NativeEvidence {
    NativeEvidence {
        native_entries: 0,
        tier2_entries: 0,
        native_exits: 0,
        unexpected_fallbacks: 0,
        opcode_ids: vec![],
        helper_ids: vec![],
    }
}
fn main() -> ExitCode {
    let args: Vec<_> = env::args().collect();
    let mode = args
        .windows(2)
        .find(|v| v[0] == "--mode")
        .and_then(|v| RunMode::from_str(&v[1]).ok())
        .unwrap_or(RunMode::Interpreter);
    let output = PathBuf::from(
        args.windows(2)
            .find(|v| v[0] == "--output")
            .map(|v| v[1].as_str())
            .unwrap_or("target/jit-quickjs/report.json"),
    );
    let limit = args
        .windows(2)
        .find(|v| v[0] == "--limit")
        .and_then(|v| v[1].parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sys/quickjs/tests");
    let mut paths = match fs::read_dir(&root) {
        Ok(v) => v
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("{}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    paths.sort();
    paths.truncate(limit);
    let compatible: Vec<String> = serde_json::from_str(include_str!(
        "../../tests/fixtures/quickjs-rust-host-compatible.json"
    ))
    .unwrap();
    let mut cases = Vec::new();
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = fs::read_to_string(&path).unwrap();
        let unsupported = (!compatible.contains(&name))
            .then_some("not in audited Rust-host-compatible subset")
            .or_else(|| {
                [
                    "import * as std",
                    "import * as os",
                    "scriptArgs",
                    "print(",
                    "console.",
                ]
                .into_iter()
                .find(|needle| source.contains(needle))
            });
        if let Some(surface) = unsupported {
            cases.push(CaseReport {
                path: name,
                variant: Test262Variant::RawScript,
                status: CaseStatus::Skip,
                duration_ms: 0,
                negative_phase: None,
                negative_type: None,
                skip_reason: Some(format!(
                    "QuickJS C-host surface `{surface}` is absent from Rust host"
                )),
                error: None,
                native: evidence(),
            });
            continue;
        }
        let start = Instant::now();
        let (result, native) = match mode {
            RunMode::Interpreter => {
                let rt = Runtime::new().unwrap();
                (
                    Context::full(&rt).unwrap().with(|ctx| {
                        ctx.eval::<(), _>(source.as_str())
                            .map_err(|e| format!("{e:?}"))
                    }),
                    evidence(),
                )
            }
            _ => {
                let rt = JitRuntime::builder().build().unwrap();
                let result = Context::full(&rt).unwrap().with(|ctx| {
                    ctx.eval::<(), _>(source.as_str())
                        .map_err(|e| format!("{e:?}"))
                });
                rt.jit().poll();
                let m = rt.metrics();
                (
                    result,
                    NativeEvidence {
                        native_entries: m.native_entries,
                        tier2_entries: m.tier2_entries,
                        native_exits: m.native_exits,
                        unexpected_fallbacks: m.native_fallbacks,
                        opcode_ids: vec![],
                        helper_ids: vec![],
                    },
                )
            }
        };
        cases.push(CaseReport {
            path: name,
            variant: Test262Variant::RawScript,
            status: if result.is_ok() {
                CaseStatus::Pass
            } else {
                CaseStatus::Fail
            },
            duration_ms: start.elapsed().as_millis() as u64,
            negative_phase: None,
            negative_type: None,
            skip_reason: None,
            error: result.err(),
            native,
        });
    }
    let report = SuiteReport::new(
        "quickjs-rust-host-compatible",
        "quickjs-pinned",
        mode,
        cases,
    );
    if let Err(e) = report.validate() {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).unwrap()
    }
    fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    let failed = report
        .cases
        .iter()
        .filter(|c| matches!(c.status, CaseStatus::Fail))
        .count();
    eprintln!(
        "{}: {} cases, {failed} failed (C run-test262 reference is reported separately)",
        output.display(),
        report.cases.len()
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
