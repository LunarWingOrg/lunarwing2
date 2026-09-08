use core::hint::black_box;
use std::time::Duration;

use char_str::{CharStr, CharString, format_char, format_char_str};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

struct Case {
    name: &'static str,
    value: &'static str,
}

const CASES: [Case; 2] = [
    Case { name: "inline", value: "name" },
    Case { name: "heap", value: "a_name_longer_than_the_inline_limit" },
];

fn char_string(c: &mut Criterion) {
    let mut group = c.benchmark_group("formatting/char_string");

    for case in &CASES {
        group.throughput(Throughput::Bytes((case.value.len() + 1) as u64));
        group.bench_function(BenchmarkId::new("format_char", case.name), |b| {
            b.iter(|| {
                let value = black_box(case.value);
                black_box(format_char!("{value}="))
            })
        });
        group.bench_function(BenchmarkId::new("format_then_convert", case.name), |b| {
            b.iter(|| {
                let value = black_box(case.value);
                black_box(CharString::from(format!("{value}=")))
            })
        });
    }

    group.finish();
}

fn char_str(c: &mut Criterion) {
    let mut group = c.benchmark_group("formatting/char_str");

    for case in &CASES {
        group.throughput(Throughput::Bytes((case.value.len() + 1) as u64));
        group.bench_function(BenchmarkId::new("format_char_str", case.name), |b| {
            b.iter(|| {
                let value = black_box(case.value);
                black_box(format_char_str!("{value}="))
            })
        });
        group.bench_function(BenchmarkId::new("format_then_convert", case.name), |b| {
            b.iter(|| {
                let value = black_box(case.value);
                black_box(CharStr::from(format!("{value}=")))
            })
        });
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(50);
    targets = char_string, char_str
}
criterion_main!(benches);
