#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn convert_with_tier(html: &str, tier_strategy: TierStrategy) -> String {
    convert(
        html,
        Some(ConversionOptions {
            tier_strategy,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion should succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_escape_any_ordered_list_number_at_a_fresh_block_start() {
    let cases = [
        ("<p>2. z</p>", "2\\. z\n", "2\\. z\n"),
        ("<p>2.<span> z</span></p>", "2\\. z\n", "2\\. z\n"),
        ("<span>2. z</span>", "2\\. z\n", "2\\. z\n"),
        ("<p><br>2. z</p>", "2\\. z\n", "2\\. z\n"),
        ("<dl><dd><br>2. z</dd></dl>", "\n2\\. z\n", "2\\. z\n"),
        ("<p>42) answer</p>", "42\\) answer\n", "42\\) answer\n"),
    ];

    for (html, tier1_expected, tier2_expected) in cases {
        assert_eq!(
            convert_with_tier(html, TierStrategy::Tier1),
            tier1_expected,
            "Tier1: {html}"
        );
        assert_eq!(
            convert_with_tier(html, TierStrategy::Tier2),
            tier2_expected,
            "Tier2: {html}"
        );
    }
}

#[test]
fn should_leave_non_markers_at_a_fresh_block_start_unchanged() {
    for tier in [TierStrategy::Tier1, TierStrategy::Tier2] {
        assert_eq!(convert_with_tier("<p>1234567890. z</p>", tier), "1234567890. z\n");
        assert_eq!(convert_with_tier("<p>2.z</p>", tier), "2.z\n");
    }
}
