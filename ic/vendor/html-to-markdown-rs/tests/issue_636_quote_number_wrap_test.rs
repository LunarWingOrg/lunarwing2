#![allow(missing_docs)]

//! Regression tests for issue #636: a numbered line after a hard break in a quote must wrap.

use html_to_markdown_rs::{ConversionOptions, NewlineStyle, TierStrategy, convert};

fn converted(newline_style: NewlineStyle) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        newline_style,
        tier_strategy: TierStrategy::Tier2,
        wrap: true,
        wrap_width: 20,
        ..ConversionOptions::default()
    };
    convert(
        "<blockquote><p>It was first described in<br>1990. That year the first browser shipped to more people and more.</p></blockquote>",
        Some(options),
    )
    .expect("conversion must succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_wrap_a_numbered_quote_line_after_a_two_space_break() {
    assert_eq!(
        converted(NewlineStyle::Spaces),
        "> It was first\n> described in  \n> 1990. That year\n> the first browser\n> shipped to more\n> people and more.\n"
    );
}

#[test]
fn should_wrap_a_numbered_quote_line_after_a_backslash_break() {
    assert_eq!(
        converted(NewlineStyle::Backslash),
        "> It was first\n> described in\\\n> 1990. That year\n> the first browser\n> shipped to more\n> people and more.\n"
    );
}

#[test]
fn should_wrap_an_overlong_ordered_marker_as_paragraph_text() {
    let options = ConversionOptions {
        extract_metadata: false,
        newline_style: NewlineStyle::Backslash,
        tier_strategy: TierStrategy::Tier2,
        wrap: true,
        wrap_width: 20,
        ..ConversionOptions::default()
    };
    let markdown = convert(
        "<blockquote><p>Before<br>123456789012345678901234567890. trailing prose should wrap now</p></blockquote>",
        Some(options),
    )
    .expect("conversion must succeed")
    .content
    .unwrap_or_default();

    assert_eq!(
        markdown,
        "> Before\\\n> 123456789012345678901234567890.\n> trailing prose\n> should wrap now\n"
    );
}
