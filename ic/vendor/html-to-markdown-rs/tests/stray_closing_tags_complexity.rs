#![allow(missing_docs)]
#![cfg(feature = "testkit")]

//! Regression test for issue #715: a run of unmatched closing tags before a `<br>` must scale
//! linearly rather than re-scan the remaining run for every tag.

use std::time::{Duration, Instant};

use html_to_markdown_rs::options::HighlightStyle;
use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

const BASE_SIZE: usize = 1_000;
const MAX_DOUBLING_RATIO: f64 = 3.0;
const MAX_MEASUREMENT_ATTEMPTS: usize = 3;
const REPEATS_PER_SAMPLE: usize = 3;

fn input(size: usize) -> String {
    format!("a\n{}<br>b", "</i>\n".repeat(size))
}

fn fastest_run(html: &str, tier_strategy: TierStrategy) -> Duration {
    let options = ConversionOptions {
        extract_metadata: false,
        highlight_style: HighlightStyle::None,
        tier_strategy,
        ..ConversionOptions::default()
    };
    (0..REPEATS_PER_SAMPLE)
        .map(|_| {
            let start = Instant::now();
            let output = convert(std::hint::black_box(html), Some(options.clone()))
                .expect("conversion must succeed")
                .content
                .unwrap_or_default();
            assert_eq!(output, "a  \nb\n");
            start.elapsed()
        })
        .min()
        .expect("at least one timing sample")
}

fn measurement(tier_strategy: TierStrategy) -> (f64, f64, f64) {
    let small = input(BASE_SIZE);
    let medium = input(BASE_SIZE * 2);
    let large = input(BASE_SIZE * 4);
    let small_secs = fastest_run(&small, tier_strategy).as_secs_f64().max(1e-6);
    let medium_secs = fastest_run(&medium, tier_strategy).as_secs_f64().max(1e-6);
    let large_secs = fastest_run(&large, tier_strategy).as_secs_f64().max(1e-6);
    (small_secs, medium_secs, large_secs)
}

fn assert_linear(tier_strategy: TierStrategy) {
    let mut failures = Vec::with_capacity(MAX_MEASUREMENT_ATTEMPTS);
    for attempt in 1..=MAX_MEASUREMENT_ATTEMPTS {
        let (small, medium, large) = measurement(tier_strategy);
        let first_ratio = medium / small;
        let second_ratio = large / medium;
        if first_ratio < MAX_DOUBLING_RATIO && second_ratio < MAX_DOUBLING_RATIO {
            return;
        }
        failures.push(format!(
            "attempt {attempt}: {small:.4}s -> {medium:.4}s -> {large:.4}s ({first_ratio:.2}x, {second_ratio:.2}x)"
        ));
    }
    panic!(
        "{tier_strategy:?} scaled quadratically across every measurement:\n{}",
        failures.join("\n")
    );
}

#[test]
fn stray_closing_tags_before_a_break_scale_linearly_in_both_tiers() {
    assert_linear(TierStrategy::Tier1);
    assert_linear(TierStrategy::Tier2);
}
