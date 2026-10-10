#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::prescan::PrescanReport;
use html_to_markdown_rs::{ConversionOptions, HighlightStyle, TierStrategy, convert, tier1};

fn options() -> ConversionOptions {
    ConversionOptions {
        extract_metadata: false,
        highlight_style: HighlightStyle::None,
        ..ConversionOptions::default()
    }
}

fn assert_tiers(html: &str, expected: &str) {
    let base = options();
    let tier1_output = tier1::run(html, &PrescanReport::default(), &base).expect("tier1 should accept the image");
    let tier2_output = convert(
        html,
        Some(ConversionOptions {
            tier_strategy: TierStrategy::Tier2,
            ..base
        }),
    )
    .expect("tier2 conversion should succeed")
    .content
    .unwrap_or_default();

    assert_eq!(tier1_output, expected);
    assert_eq!(tier2_output, expected);
}

#[test]
fn should_preserve_an_image_alt_backslash_before_ascii_punctuation() {
    assert_tiers(r#"<p><img src="i.png" alt="a\*b"></p>"#, "![a\\\\*b](i.png)\n");
}

#[test]
fn should_leave_an_image_alt_backslash_before_a_letter_unchanged() {
    assert_tiers(r#"<p><img src="i.png" alt="a\b"></p>"#, "![a\\b](i.png)\n");
}
