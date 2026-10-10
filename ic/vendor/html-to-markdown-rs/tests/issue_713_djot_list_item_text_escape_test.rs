// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, OutputFormat, TierStrategy, convert};

fn convert_with(html: &str, tier_strategy: TierStrategy) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        output_format: OutputFormat::Djot,
        tier_strategy,
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion must succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_escape_numbered_text_at_the_start_of_a_djot_list_item() {
    for (html, expected) in [
        ("<ol><li>2. z</li></ol>", "1. 2\\. z\n"),
        ("<ol><li>2.<span> z</span></li></ol>", "1. 2\\. z\n"),
        ("<ul><li>42) z</li></ul>", "- 42\\) z\n"),
        ("<ul><li>123456789012. z</li></ul>", "- 123456789012\\. z\n"),
        ("<ol><li>1. z</li></ol>", "1. 1\\. z\n"),
    ] {
        assert_eq!(convert_with(html, TierStrategy::Tier1), expected, "tier 1: {html:?}");
        assert_eq!(convert_with(html, TierStrategy::Tier2), expected, "tier 2: {html:?}");
    }
}

#[test]
fn should_not_escape_numbered_text_away_from_the_item_start() {
    let html = "<ol><li>x 2. z</li></ol>";
    let expected = "1. x 2. z\n";
    assert_eq!(convert_with(html, TierStrategy::Tier1), expected);
    assert_eq!(convert_with(html, TierStrategy::Tier2), expected);
}
