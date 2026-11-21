use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rquickjs::{Context, Runtime};

fn micro(c: &mut Criterion) {
    c.bench_function("interpreter/add-loop", |b| {
        b.iter(|| {
            let runtime = Runtime::new().unwrap();
            let context = Context::full(&runtime).unwrap();
            context
                .with(|ctx| ctx.eval::<i32, _>(black_box("let s=0;for(let i=0;i<1000;i++)s+=i;s")))
                .unwrap()
        })
    });
}
criterion_group!(benches, micro);
criterion_main!(benches);
