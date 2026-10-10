#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::prescan::PrescanReport;
use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn markdown(html: &str, tier_strategy: TierStrategy) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        tier_strategy,
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_indent_a_link_line_after_a_hard_break_to_the_item_content_column() {
    let html = "<ul><li><a href=\"u\">a<br>2. t</a></li></ul>";
    let expected = "- [a  \n  2. t](u)\n";

    assert_eq!(markdown(html, TierStrategy::Tier2), expected);
    let tier1_options = ConversionOptions {
        extract_metadata: false,
        ..ConversionOptions::default()
    };
    assert_eq!(
        html_to_markdown_rs::tier1::run(html, &PrescanReport::default(), &tier1_options)
            .expect("tier 1 should convert this input"),
        expected
    );
    let rendered = comrak::markdown_to_html(expected, &comrak::Options::default());
    assert!(rendered.contains("<a href=\"u\">a<br />\n2. t</a>"), "{rendered:?}");
    assert!(!rendered.contains("<ol"), "{rendered:?}");
}
