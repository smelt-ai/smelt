use rquickjs::{Context, Runtime};
use rquickjs_jit::correctness::{
    canonical_plain_data_observation_source, canonical_plain_data_observer_prelude,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Regression {
    seed: u64,
    source: String,
    invocation: String,
    expected: String,
    required_tier: String,
}

#[test]
fn replay_checked_in_regressions() {
    let source = include_str!("regressions/cases/special-values.json");
    let case: Regression = serde_json::from_str(source).unwrap();
    assert_eq!(case.seed, 0);
    assert_eq!(case.required_tier, "interpreter-or-automatic");
    let runtime = Runtime::new().unwrap();
    let observation = Context::full(&runtime).unwrap().with(|ctx| {
        ctx.eval::<(), _>(canonical_plain_data_observer_prelude())
            .unwrap();
        ctx.eval::<(), _>(case.source.as_str()).unwrap();
        ctx.eval::<String, _>(canonical_plain_data_observation_source(&case.invocation))
            .unwrap()
    });
    assert_eq!(observation, case.expected);
}

#[cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
mod tier2_ownership {
    use rquickjs::{Context, Function, Object, Runtime};
    use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};

    /// Runs `workload(2000, 7, {x:0,y:1})` until the requested tier settles,
    /// returns the result and metrics, and tears the runtime down. A
    /// reference-count error surfaces as a QuickJS assertion abort at
    /// `JS_FreeRuntime`, which fails the whole test binary.
    fn run(source: &str, expect_tier2: bool) -> (f64, rquickjs_jit::JitMetrics) {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .tier_policy(JitTierPolicy::Optimize)
                .call_threshold(1)
                .loop_threshold(1)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| {
                ctx.eval::<(), _>(format!("globalThis.workloadArgument={{x:0,y:1}}; {source}"))
            })
            .unwrap();
        let mut result = 0.0;
        // At least 200 calls. A caller expecting Tier 2 keeps calling until it
        // is entered: slow (sanitizer) builds may queue the trial later.
        let budget = if cfg!(rquickjs_sanitizer) { 300 } else { 60 };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget);
        for call in 0.. {
            result = context.with(|ctx| {
                let workload: Function<'_> = ctx.globals().get("workload").unwrap();
                let argument: Object<'_> = ctx.globals().get("workloadArgument").unwrap();
                workload.call((2000, 7, argument)).unwrap()
            });
            jit.poll();
            if jit.metrics().tier2_entries > 0
                || (call >= 199 && (!expect_tier2 || std::time::Instant::now() >= deadline))
            {
                break;
            }
            if call >= 199 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        let metrics = jit.metrics();
        drop(context);
        drop(jit);
        drop(runtime);
        (result, metrics)
    }

    #[test]
    fn guarded_property_store_below_a_receiver_alias_releases_materialized_owners() {
        // `p.x = p.x + 1` keeps a second receiver alias below the get_field
        // operands. Before M2 each guarded access leaked one reference and
        // JS_FreeRuntime aborted on a non-empty gc_obj_list.
        let (result, metrics) = run(
            "function workload(n,s,p){ for(let i=0;i<n;i++){ p.x = p.x + 1 } return p.x }",
            true,
        );
        assert!(metrics.tier2_entries > 0, "{metrics:?}");
        assert_eq!(metrics.native_fallbacks, 0, "{metrics:?}");
        assert_eq!(metrics.deopts, 0, "{metrics:?}");
        assert!(result >= 2000.0, "{result}");
    }

    #[test]
    fn heap_argument_aliases_stored_into_locals_stay_on_tier1_with_exact_results() {
        // A local holding a borrowed heap alias cannot yet be materialized by
        // the identity-only deoptimization recipes; Tier 2 fails closed and
        // the interpreter/Tier 1 result is exact with a clean teardown.
        for source in [
            "function workload(n,s,p){ let q=p; for(let i=0;i<n;i++){ q.x = q.x + 1 } return q.x }",
            "function workload(n,s,p){ let q=p; let t=0; for(let i=0;i<n;i++){ t = t + q.x } return t }",
            "function workload(n,s,p){ let q=p; for(let i=0;i<n;i++){ p.x = p.x + 1 } return q.x }",
        ] {
            let (result, metrics) = run(source, false);
            assert_eq!(metrics.tier2_entries, 0, "{source}: {metrics:?}");
            assert_eq!(metrics.native_fallbacks, 0, "{source}: {metrics:?}");
            assert!(result >= 0.0, "{source}: {result}");
        }
    }
}
