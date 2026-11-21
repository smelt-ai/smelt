use criterion::{criterion_group, criterion_main, Criterion};
use rquickjs::{Context, Runtime};
use rquickjs_jit::{Jit, JitConfig};

fn tiering(c: &mut Criterion) {
    c.bench_function("automatic/tier-up", |b| {
        b.iter(|| {
            let runtime = Runtime::new().unwrap();
            let jit = Jit::attach(&runtime, JitConfig::default()).unwrap();
            let context = Context::full(&runtime).unwrap();
            context
                .with(|ctx| {
                    ctx.eval::<f64, _>(
                        "function f(n,z){let s=z;for(let i=z;i<n;i++)s+=i;return s}f(20000,0)",
                    )
                })
                .unwrap();
            jit.poll();
            jit.metrics().native_entries
        })
    });
}
criterion_group!(benches, tiering);
criterion_main!(benches);
