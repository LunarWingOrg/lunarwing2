use core::{cmp::Ordering, hint::black_box};
use std::time::Duration;

use char_str::{CharStr, CharString};
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main,
    measurement::WallTime,
};

const STRING_LENGTHS: [usize; 4] = [8, 17, 64, 4096];

fn text_with_byte(len: usize, index: usize, byte: u8) -> String {
    let mut text = vec![b'a'; len];
    text[index] = byte;
    String::from_utf8(text).unwrap()
}

fn bench_eq<T: PartialEq>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    case: &str,
    len: usize,
    left: &T,
    right: &T,
) {
    group.bench_function(BenchmarkId::new(case, len), |b| {
        b.iter(|| black_box(left).eq(black_box(right)));
    });
}

fn bench_cmp<T: Ord>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    case: &str,
    len: usize,
    left: &T,
    right: &T,
) {
    group.bench_function(BenchmarkId::new(case, len), |b| {
        b.iter(|| black_box(left).cmp(black_box(right)));
    });
}

fn equality(c: &mut Criterion) {
    let mut group = c.benchmark_group("comparison/equality");

    for len in STRING_LENGTHS {
        group.throughput(Throughput::Bytes(len as u64));

        let text = "a".repeat(len);
        let first_mismatch = text_with_byte(len, 0, b'b');
        let last_mismatch = text_with_byte(len, len - 1, b'b');

        let exact = CharStr::from(text.as_str());
        let exact_equal = CharStr::from(text.as_str());
        let exact_first_mismatch = CharStr::from(first_mismatch.as_str());
        let exact_last_mismatch = CharStr::from(last_mismatch.as_str());

        bench_eq(&mut group, "char_str/distinct_equal", len, &exact, &exact_equal);
        bench_eq(&mut group, "char_str/first_byte_mismatch", len, &exact, &exact_first_mismatch);
        bench_eq(&mut group, "char_str/last_byte_mismatch", len, &exact, &exact_last_mismatch);

        let growable = CharString::from(text.as_str());
        let growable_equal = CharString::from(text.as_str());
        let growable_first_mismatch = CharString::from(first_mismatch.as_str());
        let growable_last_mismatch = CharString::from(last_mismatch.as_str());

        bench_eq(&mut group, "char_string/distinct_equal", len, &growable, &growable_equal);
        bench_eq(
            &mut group,
            "char_string/first_byte_mismatch",
            len,
            &growable,
            &growable_first_mismatch,
        );
        bench_eq(
            &mut group,
            "char_string/last_byte_mismatch",
            len,
            &growable,
            &growable_last_mismatch,
        );

        if len > size_of::<CharStr>() {
            let exact_shared = exact.clone();
            assert!(core::ptr::eq(exact.as_ptr(), exact_shared.as_ptr()));
            bench_eq(&mut group, "char_str/shared", len, &exact, &exact_shared);

            let growable_shared = growable.clone();
            assert!(core::ptr::eq(growable.as_ptr(), growable_shared.as_ptr()));
            bench_eq(&mut group, "char_string/shared", len, &growable, &growable_shared);
        }
    }

    group.finish();
}

fn ordering(c: &mut Criterion) {
    let mut group = c.benchmark_group("comparison/ordering");

    for len in STRING_LENGTHS {
        group.throughput(Throughput::Bytes(len as u64));

        let text = "a".repeat(len);
        let first_mismatch = text_with_byte(len, 0, b'b');
        let last_mismatch = text_with_byte(len, len - 1, b'b');

        let exact = CharStr::from(text.as_str());
        let exact_equal = CharStr::from(text.as_str());
        let exact_first_mismatch = CharStr::from(first_mismatch.as_str());
        let exact_last_mismatch = CharStr::from(last_mismatch.as_str());

        bench_cmp(&mut group, "char_str/distinct_equal", len, &exact, &exact_equal);
        bench_cmp(&mut group, "char_str/first_byte_mismatch", len, &exact, &exact_first_mismatch);
        bench_cmp(&mut group, "char_str/last_byte_mismatch", len, &exact, &exact_last_mismatch);

        let growable = CharString::from(text.as_str());
        let growable_equal = CharString::from(text.as_str());
        let growable_first_mismatch = CharString::from(first_mismatch.as_str());
        let growable_last_mismatch = CharString::from(last_mismatch.as_str());

        bench_cmp(&mut group, "char_string/distinct_equal", len, &growable, &growable_equal);
        bench_cmp(
            &mut group,
            "char_string/first_byte_mismatch",
            len,
            &growable,
            &growable_first_mismatch,
        );
        bench_cmp(
            &mut group,
            "char_string/last_byte_mismatch",
            len,
            &growable,
            &growable_last_mismatch,
        );

        if len > size_of::<CharStr>() {
            let exact_shared = exact.clone();
            assert!(core::ptr::eq(exact.as_ptr(), exact_shared.as_ptr()));
            bench_cmp(&mut group, "char_str/shared", len, &exact, &exact_shared);

            let growable_shared = growable.clone();
            assert!(core::ptr::eq(growable.as_ptr(), growable_shared.as_ptr()));
            bench_cmp(&mut group, "char_string/shared", len, &growable, &growable_shared);

            let mut growable_prefix = growable.clone();
            growable_prefix.truncate(len - 1);
            assert!(core::ptr::eq(growable.as_ptr(), growable_prefix.as_ptr()));
            assert_eq!(growable.as_str().cmp(growable_prefix.as_str()), Ordering::Greater);
            bench_cmp(&mut group, "char_string/shared_prefix", len, &growable, &growable_prefix);
        }
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(50);
    targets = equality, ordering
}
criterion_main!(benches);
