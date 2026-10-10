// ~keep The inner attributes below are crate-level Rust attributes, not a shell shebang.
#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::options::HighlightStyle;
use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn convert_with(html: &str, tier_strategy: TierStrategy) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        highlight_style: HighlightStyle::None,
        tier_strategy,
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion must succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_keep_form_elements_inline_before_a_hard_break() {
    for (html, expected) in [
        ("<label>First</label><br>Second", "First  \nSecond\n"),
        ("<select><option>x</option></select><br>y", "x  \ny\n"),
    ] {
        assert_eq!(convert_with(html, TierStrategy::Tier1), expected, "tier 1: {html:?}");
        assert_eq!(convert_with(html, TierStrategy::Tier2), expected, "tier 2: {html:?}");
    }
}

#[test]
fn should_drop_a_trailing_source_newline_inside_abbr_and_kbd_before_a_hard_break() {
    for (html, expected) in [
        (
            r#"<abbr title="t">First
</abbr><br>Second"#,
            "First (t)  \nSecond\n",
        ),
        ("<kbd>First\n</kbd><br>Second", "`First`  \nSecond\n"),
    ] {
        assert_eq!(convert_with(html, TierStrategy::Tier1), expected, "tier 1: {html:?}");
        assert_eq!(convert_with(html, TierStrategy::Tier2), expected, "tier 2: {html:?}");
    }
}
