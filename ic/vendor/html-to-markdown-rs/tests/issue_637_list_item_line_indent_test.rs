#![allow(missing_docs)]

//! Regression tests for issue #637: every continuation line must stay at the item's column.

use html_to_markdown_rs::{ConversionOptions, NewlineStyle, TierStrategy, convert};

fn converted(html: &str, newline_style: NewlineStyle, wrap_width: Option<usize>) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        newline_style,
        tier_strategy: TierStrategy::Tier2,
        wrap: wrap_width.is_some(),
        wrap_width: wrap_width.unwrap_or(80),
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion must succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_indent_a_backslash_break_line_at_the_item_column() {
    let html = "<ul><li>aa<br>\n<br>--- b c d 10. e</li></ul>";
    assert_eq!(
        converted(html, NewlineStyle::Backslash, None),
        "- aa\\\n  \\\n  --- b c d 10. e\n"
    );
}

#[test]
fn should_indent_a_source_line_continuation_at_the_item_column() {
    let html = "<blockquote><ul><li>aa bb\n aa 1.</li></ul></blockquote>";
    let expected = "> - aa bb\n>   aa 1.\n";

    assert_eq!(converted(html, NewlineStyle::Spaces, None), expected);
    #[cfg(feature = "testkit")]
    {
        let tier1_options = ConversionOptions {
            extract_metadata: false,
            tier_strategy: TierStrategy::Tier1,
            ..ConversionOptions::default()
        };
        assert_eq!(
            convert(html, Some(tier1_options))
                .expect("conversion must succeed")
                .content
                .unwrap_or_default(),
            expected
        );
    }
}

#[test]
fn should_keep_wrapped_continuations_inside_the_list_item() {
    let html = "<blockquote><ul><li>aa bb\n aa 1.</li></ul></blockquote>";
    let markdown = converted(html, NewlineStyle::Spaces, Some(1));
    let rendered = comrak::markdown_to_html(&markdown, &comrak::Options::default());
    assert_eq!(
        rendered.matches("<ul>").count(),
        1,
        "{markdown:?} rendered as {rendered:?}"
    );
    assert_eq!(
        rendered.matches("<li>").count(),
        1,
        "{markdown:?} rendered as {rendered:?}"
    );
    assert!(!rendered.contains("<ol>"), "{markdown:?} rendered as {rendered:?}");
}
