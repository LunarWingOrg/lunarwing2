#![allow(missing_docs)]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

#[test]
fn should_not_push_item_text_out_after_an_empty_nested_list() {
    let options = ConversionOptions {
        extract_metadata: false,
        tier_strategy: TierStrategy::Tier2,
        ..ConversionOptions::default()
    };
    let html = "<ul><li><ol start=\"2\"></ol>x</li><li></li></ul>";
    let actual = convert(html, Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default();

    assert_eq!(actual, "- x\n-\n");
}
