#![allow(missing_docs)]
#![cfg(feature = "testkit")]

//! Regression test for issue #594: text before `<head>` must be discarded by both tiers.

use html_to_markdown_rs::prescan::PrescanReport;
use html_to_markdown_rs::tier1;
use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn options(tier_strategy: TierStrategy) -> ConversionOptions {
    ConversionOptions {
        extract_metadata: true,
        tier_strategy,
        ..ConversionOptions::default()
    }
}

#[test]
fn should_discard_a_non_breaking_space_before_the_head_in_both_tiers() {
    let html = "&nbsp;<head><title>T</title></head><p>x</p>";
    let tier1 = tier1::run(html, &PrescanReport::default(), &options(TierStrategy::Tier1))
        .expect("Tier 1 must convert the input");
    let tier2 = convert(html, Some(options(TierStrategy::Tier2)))
        .expect("Tier 2 must convert the input")
        .content
        .unwrap_or_default();

    assert_eq!(tier1, tier2);
    assert!(
        !tier1.contains('\u{a0}'),
        "pre-head text leaked into the body: {tier1:?}"
    );
    assert!(tier1.ends_with("x\n"), "the body was lost: {tier1:?}");
}
