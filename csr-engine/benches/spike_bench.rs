use criterion::{criterion_group, criterion_main, Criterion};

fn bench_placeholder(c: &mut Criterion) {
    c.bench_function("placeholder", |b| {
        b.iter(|| {
            // Will be replaced with real benchmarks after cargo check passes
            let _ = 1 + 1;
        });
    });
}

criterion_group!(benches, bench_placeholder);
criterion_main!(benches);
