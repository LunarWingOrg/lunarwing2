use core::hint::black_box;
use std::time::Duration;

use char_str::{CharStr, CharString};
use criterion::{
    BatchSize, BenchmarkGroup, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main,
    measurement::WallTime,
};

const STRING_LENGTHS: [usize; 10] = [11, 12, 13, 15, 16, 17, 24, 32, 48, 64];
const VECTOR_LEN: usize = 16_384;

fn construction(c: &mut Criterion) {
    let mut construction = c.benchmark_group("heap_layout/construction");

    for len in STRING_LENGTHS {
        let text = "a".repeat(len);

        construction.throughput(Throughput::Elements(VECTOR_LEN as u64));
        construction.bench_function(BenchmarkId::from_parameter(len), |b| {
            b.iter(|| {
                black_box(
                    (0..VECTOR_LEN)
                        .map(|_| CharString::from(black_box(text.as_str())))
                        .collect::<Vec<_>>(),
                )
            });
        });
    }

    construction.finish();
}

fn clone_drop(c: &mut Criterion) {
    let mut clone_drop = c.benchmark_group("heap_layout/clone_drop");

    for len in STRING_LENGTHS {
        let text = "a".repeat(len);
        let values = (0..VECTOR_LEN).map(|_| CharString::from(text.as_str())).collect::<Vec<_>>();

        clone_drop.throughput(Throughput::Elements(VECTOR_LEN as u64));
        clone_drop.bench_function(BenchmarkId::from_parameter(len), |b| {
            b.iter(|| black_box(black_box(&values).clone()));
        });
    }

    clone_drop.finish();
}

fn immutable_construction(c: &mut Criterion) {
    let mut construction = c.benchmark_group("heap_layout/char_str_construction");

    for len in STRING_LENGTHS {
        let text = "a".repeat(len);

        construction.throughput(Throughput::Elements(VECTOR_LEN as u64));
        construction.bench_function(BenchmarkId::from_parameter(len), |b| {
            b.iter(|| {
                black_box(
                    (0..VECTOR_LEN)
                        .map(|_| CharStr::from(black_box(text.as_str())))
                        .collect::<Vec<_>>(),
                )
            });
        });
    }

    construction.finish();
}

fn immutable_clone_drop(c: &mut Criterion) {
    let mut clone_drop = c.benchmark_group("heap_layout/char_str_clone_drop");

    for len in STRING_LENGTHS {
        let text = "a".repeat(len);
        let values = (0..VECTOR_LEN).map(|_| CharStr::from(text.as_str())).collect::<Vec<_>>();

        clone_drop.throughput(Throughput::Elements(VECTOR_LEN as u64));
        clone_drop.bench_function(BenchmarkId::from_parameter(len), |b| {
            b.iter(|| black_box(black_box(&values).clone()));
        });
    }

    clone_drop.finish();
}

fn bench_drop<T>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name: &str,
    mut setup: impl FnMut() -> Vec<T>,
) {
    group.bench_function(name, |b| {
        b.iter_batched(
            &mut setup,
            // Keep destruction inside the instrumented routine. CodSpeed drops values returned
            // by `iter_batched` after ending the measurement.
            |values| drop(black_box(values)),
            BatchSize::LargeInput,
        );
    });
}

fn drop_vectors(c: &mut Criterion) {
    let mut drop_group = c.benchmark_group("heap_layout/drop");
    drop_group.throughput(Throughput::Elements(VECTOR_LEN as u64));

    let inline_string = CharString::from("inline");
    bench_drop(&mut drop_group, "char_string/inline", || vec![inline_string.clone(); VECTOR_LEN]);

    let heap_text = "a".repeat(64);
    bench_drop(&mut drop_group, "char_string/unique_heap", || {
        (0..VECTOR_LEN).map(|_| CharString::from(heap_text.as_str())).collect()
    });

    let shared_string = CharString::from(heap_text.as_str());
    bench_drop(&mut drop_group, "char_string/shared_heap", || {
        vec![shared_string.clone(); VECTOR_LEN]
    });

    let inline_str = CharStr::from("inline");
    bench_drop(&mut drop_group, "char_str/inline", || vec![inline_str.clone(); VECTOR_LEN]);

    bench_drop(&mut drop_group, "char_str/unique_heap", || {
        (0..VECTOR_LEN).map(|_| CharStr::from(heap_text.as_str())).collect()
    });

    let shared_str = CharStr::from(heap_text.as_str());
    bench_drop(&mut drop_group, "char_str/shared_heap", || vec![shared_str.clone(); VECTOR_LEN]);

    drop_group.finish();
}

fn traversal(c: &mut Criterion) {
    let mut traversal = c.benchmark_group("heap_layout/traversal");

    for len in STRING_LENGTHS {
        let text = "a".repeat(len);
        let values = (0..VECTOR_LEN).map(|_| CharString::from(text.as_str())).collect::<Vec<_>>();

        traversal.throughput(Throughput::Bytes((VECTOR_LEN * len) as u64));
        traversal.bench_function(BenchmarkId::from_parameter(len), |b| {
            b.iter(|| {
                black_box(&values)
                    .iter()
                    .map(|value| usize::from(value.as_bytes()[0]) + value.len())
                    .sum::<usize>()
            });
        });
    }

    traversal.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(50);
    targets = construction, clone_drop, immutable_construction, immutable_clone_drop, drop_vectors, traversal
}
criterion_main!(benches);
