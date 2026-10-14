//! Detector cost: 10,000 clean 900-byte components and 10,000 components that
//! carry CSR text. Budget: p95 at most 100 µs per component
//! (`import::contamination::tests::detection_p95_stays_inside_the_budget`
//! asserts it on every test run; this reports the distribution).

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

use csr_engine::import::contamination::has_csr_text;

const COMPONENTS: usize = 10_000;

fn clean_component() -> String {
    let text = "We fixed the stale HNSW vector by replacing it explicitly, then reran the suite. "
        .repeat(12);
    text[..900].to_string()
}

fn matching_component() -> String {
    format!(
        "{}\n\n{}\n\n{}",
        "Please look at this and tell me what you think. ".repeat(6),
        "recap [4m ago]: Fixed the importer. [[CSR:RECAP]]\nSettled: a (b).\nNext: c.",
        "CSR CONTINUUM [4m ago]: Fix\nLAST: x (outcome=success, trust=user_history)\nFull state: csr_reflect_on_past(\"conv_a\")",
    )
}

fn classify(text: &str) -> bool {
    has_csr_text(text)
}

fn bench_classification(c: &mut Criterion) {
    let clean: Vec<String> = (0..COMPONENTS).map(|_| clean_component()).collect();
    let matching: Vec<String> = (0..COMPONENTS).map(|_| matching_component()).collect();
    let mut group = c.benchmark_group("contamination_classification");
    group.bench_function("clean_900B_x10000", |b| {
        b.iter(|| {
            clean
                .iter()
                .filter(|text| classify(black_box(text)))
                .count()
        })
    });
    group.bench_function("matching_x10000", |b| {
        b.iter(|| {
            matching
                .iter()
                .filter(|text| classify(black_box(text)))
                .count()
        })
    });
    group.bench_function("clean_900B_single", |b| {
        b.iter(|| classify(black_box(&clean[0])))
    });
    group.bench_function("matching_single", |b| {
        b.iter(|| classify(black_box(&matching[0])))
    });
    group.finish();
}

criterion_group!(benches, bench_classification);
criterion_main!(benches);
