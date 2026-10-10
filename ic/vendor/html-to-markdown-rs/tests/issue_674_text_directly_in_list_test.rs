#![allow(missing_docs)]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn markdown(html: &str) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        tier_strategy: TierStrategy::Tier2,
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_escape_a_list_marker_written_as_direct_list_text() {
    assert_eq!(markdown("y<ol>1.</ol>"), "y\n\n1\\.\n");
}

#[test]
fn should_separate_direct_list_text_from_the_following_text() {
    assert_eq!(markdown("<ul>x</ul>z"), "x\n\nz\n");
}

#[test]
fn should_escape_direct_list_text_inside_a_quote_in_an_item() {
    let html = "<ol start=\"2\"><blockquote>y<ol>1.</ol></blockquote></ol>";
    assert_eq!(markdown(html), "> y\n>\n> 1\\.\n");
}
