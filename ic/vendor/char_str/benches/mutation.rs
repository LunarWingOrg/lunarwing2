use core::hint::black_box;
use std::time::Duration;

use char_str::CharString;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

fn reserve_and_push_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("reserve_and_push_paths");

    group.bench_function("push_str/inline_capacity", |b| {
        b.iter_batched(
            || CharString::from("inline"),
            |mut value| {
                value.push_str(black_box("x"));
                black_box(value)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("push_str/unique_heap_capacity", |b| {
        b.iter_batched(
            || {
                let mut value = CharString::with_capacity(256);
                value.push_str("heap-backed text");
                value
            },
            |mut value| {
                value.push_str(black_box("x"));
                black_box(value)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("push_str/unique_heap_growth", |b| {
        let text = "a".repeat(64);
        b.iter_batched(
            || CharString::from(text.as_str()),
            |mut value| {
                value.push_str(black_box("0123456789abcdef"));
                black_box(value)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("push_str/shared_detach", |b| {
        let text = "a".repeat(64);
        let original = CharString::from(text.as_str());
        b.iter_batched(
            || (original.clone(), original.clone()),
            |(mut value, shared)| {
                value.push_str(black_box("x"));
                black_box(shared);
                black_box(value)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("push_str/static_mutation", |b| {
        b.iter_batched(
            || CharString::from_static_str("a static string longer than inline storage"),
            |mut value| {
                value.push_str(black_box("x"));
                black_box(value)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("reserve/inline_capacity", |b| {
        b.iter_batched(
            || CharString::from("inline"),
            |mut value| {
                value.reserve(black_box(1));
                black_box(value)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("reserve/unique_heap_capacity", |b| {
        b.iter_batched(
            || {
                let mut value = CharString::with_capacity(256);
                value.push_str("heap-backed text");
                value
            },
            |mut value| {
                value.reserve(black_box(1));
                black_box(value)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(50);
    targets = reserve_and_push_paths
}
criterion_main!(benches);
