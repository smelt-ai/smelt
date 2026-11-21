use criterion::{criterion_group, criterion_main, Criterion};
use rquickjs::{Context, Runtime};

fn algorithms(c: &mut Criterion) {
    c.bench_function("interpreter/numeric-kernel", |b| {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| {
                ctx.eval::<(), _>("function f(n,z){let s=z;for(let i=z;i<n;i++)s+=i;return s}")
            })
            .unwrap();
        b.iter(|| {
            context
                .with(|ctx| ctx.eval::<f64, _>("f(20000,0)"))
                .unwrap()
        })
    });
}
criterion_group!(benches, algorithms);
criterion_main!(benches);
