#![allow(missing_docs)]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

#[test]
fn should_keep_a_preserved_custom_element_and_following_heading_inside_the_item() {
    let options = ConversionOptions {
        extract_metadata: false,
        preserve_tags: vec!["my-el".to_string()],
        tier_strategy: TierStrategy::Tier2,
        ..ConversionOptions::default()
    };
    let html = "<ol><li><p>a</p><my-el></my-el><h2>h</h2>t</li></ol>";
    let actual = convert(html, Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default();

    assert_eq!(actual, "1. a\n\n   <my-el></my-el>\n\n   ## h\n   t\n");
}

#[test]
fn should_keep_a_top_level_preserved_custom_element_inline() {
    let options = ConversionOptions {
        extract_metadata: false,
        preserve_tags: vec!["my-el".to_string()],
        tier_strategy: TierStrategy::Tier2,
        ..ConversionOptions::default()
    };
    let actual = convert("<my-el>x</my-el>z", Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default();

    assert_eq!(actual, "<my-el>x</my-el>z\n");
}
