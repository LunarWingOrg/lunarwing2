#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn assert_all_tiers(html: &str, expected: &str) {
    for tier_strategy in [TierStrategy::Auto, TierStrategy::Tier1, TierStrategy::Tier2] {
        let options = ConversionOptions {
            tier_strategy,
            ..ConversionOptions::default()
        };
        let output = convert(html, Some(options))
            .expect("conversion should succeed")
            .content
            .unwrap_or_default();
        assert_eq!(output, expected, "unexpected output for {tier_strategy:?}");
    }
}

#[test]
fn should_drop_a_leading_break_before_plain_text_in_a_paragraph() {
    assert_all_tiers("<p><br>B</p>", "B\n");
}

#[test]
fn should_drop_a_leading_break_before_a_span_in_a_paragraph() {
    assert_all_tiers("<p><br><span>B</span></p>", "B\n");
}

#[test]
fn should_drop_a_leading_break_inside_inline_wrappers_at_paragraph_start() {
    for (html, expected) in [
        ("<p><em><br>B</em></p>", "*B*\n"),
        ("<p><strong><br>B</strong></p>", "**B**\n"),
        ("<p><mark><br>B</mark></p>", "==B==\n"),
        ("<p><sub><br>B</sub></p>", "B\n"),
    ] {
        assert_all_tiers(html, expected);
    }
}

#[test]
fn should_keep_a_break_inside_inline_wrappers_after_paragraph_text() {
    for (html, expected) in [
        ("<p>A<em><br>B</em></p>", "A  \n*B*\n"),
        ("<p>A<strong><br>B</strong></p>", "A  \n**B**\n"),
        ("<p>A<mark><br>B</mark></p>", "A  \n==B==\n"),
        ("<p>A<sub><br>B</sub></p>", "A  \nB\n"),
    ] {
        assert_all_tiers(html, expected);
    }
}

#[test]
fn should_keep_a_break_after_content_inside_emphasis_and_strong() {
    for (html, expected) in [
        ("<p><em>A<br>B</em></p>", "*A  \nB*\n"),
        ("<p><strong>A<br>B</strong></p>", "**A  \nB**\n"),
    ] {
        assert_all_tiers(html, expected);
    }
}

#[test]
fn should_keep_a_break_after_content_inside_nested_inline_wrappers() {
    for (html, expected) in [
        ("<p><em><strong>A<br>B</strong></em></p>", "***A  \nB***\n"),
        ("<p><strong><em>A<br>B</em></strong></p>", "***A  \nB***\n"),
    ] {
        assert_all_tiers(html, expected);
    }
}

#[test]
fn should_drop_a_leading_break_inside_nested_inline_wrappers() {
    for html in [
        "<p><em><strong><br>B</strong></em></p>",
        "<p><strong><em><br>B</em></strong></p>",
    ] {
        assert_all_tiers(html, "***B***\n");
    }
}

#[test]
fn should_keep_a_leading_top_level_break() {
    assert_all_tiers("<br>B", "\nB\n");
}
