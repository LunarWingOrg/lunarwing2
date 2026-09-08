use core::hint::black_box;
use std::time::Duration;

use char_str::CharString;
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

const MOVE_COLLECTION_COUNTS: [usize; 5] = [0, 1, 2, 8, 64];

fn segment(index: usize, len: usize) -> String {
    let byte = b'a' + (index % 26) as u8;
    String::from_utf8(vec![byte; len]).unwrap()
}

fn char_segments(count: usize, len: usize) -> Vec<CharString> {
    (0..count).map(|index| CharString::from(segment(index, len))).collect()
}

fn reserved_first_segments(count: usize, len: usize) -> Vec<CharString> {
    if count == 0 {
        return Vec::new();
    }

    let mut segments = Vec::with_capacity(count);
    let mut first = CharString::with_capacity(count * len);
    first.push_str(&segment(0, len));
    segments.push(first);
    segments.extend((1..count).map(|index| CharString::from(segment(index, len))));
    segments
}

fn shared_first_segments(count: usize, len: usize) -> (CharString, Vec<CharString>) {
    if count == 0 {
        return (CharString::new(), Vec::new());
    }

    let owner = CharString::from(segment(0, len));
    let mut segments = Vec::with_capacity(count);
    segments.push(owner.clone());
    segments.extend((1..count).map(|index| CharString::from(segment(index, len))));
    (owner, segments)
}

fn move_collection(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_collection");

    for count in MOVE_COLLECTION_COUNTS {
        group.bench_function(BenchmarkId::new("char_inline", count), |b| {
            b.iter_batched(
                || char_segments(count, 8),
                |segments| black_box(segments.into_iter().collect::<CharString>()),
                BatchSize::SmallInput,
            );
        });

        group.bench_function(BenchmarkId::new("string_inline", count), |b| {
            b.iter_batched(
                || (0..count).map(|index| segment(index, 8)).collect::<Vec<_>>(),
                |segments| black_box(segments.into_iter().collect::<String>()),
                BatchSize::SmallInput,
            );
        });

        group.bench_function(BenchmarkId::new("char_heap", count), |b| {
            b.iter_batched(
                || char_segments(count, 32),
                |segments| black_box(segments.into_iter().collect::<CharString>()),
                BatchSize::SmallInput,
            );
        });

        group.bench_function(BenchmarkId::new("string_heap", count), |b| {
            b.iter_batched(
                || (0..count).map(|index| segment(index, 32)).collect::<Vec<_>>(),
                |segments| black_box(segments.into_iter().collect::<String>()),
                BatchSize::SmallInput,
            );
        });

        group.bench_function(BenchmarkId::new("char_empty", count), |b| {
            b.iter_batched(
                || vec![CharString::new(); count],
                |segments| black_box(segments.into_iter().collect::<CharString>()),
                BatchSize::SmallInput,
            );
        });

        group.bench_function(BenchmarkId::new("char_reserved_first", count), |b| {
            b.iter_batched(
                || reserved_first_segments(count, 32),
                |segments| black_box(segments.into_iter().collect::<CharString>()),
                BatchSize::SmallInput,
            );
        });

        group.bench_function(BenchmarkId::new("char_shared_first", count), |b| {
            b.iter_batched(
                || shared_first_segments(count, 32),
                |(owner, segments)| {
                    let output = segments.into_iter().collect::<CharString>();
                    black_box(owner);
                    black_box(output)
                },
                BatchSize::SmallInput,
            );
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
    targets = move_collection
}
criterion_main!(benches);
