#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, ListIndentType, TierStrategy, convert};

fn convert_with(html: &str, tier_strategy: TierStrategy, list_indent_type: ListIndentType) -> String {
    convert(
        html,
        Some(ConversionOptions {
            extract_metadata: false,
            tier_strategy,
            list_indent_type,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion must succeed")
    .content
    .unwrap_or_default()
}

fn option_matrix() -> impl Iterator<Item = (TierStrategy, ListIndentType)> {
    [TierStrategy::Tier1, TierStrategy::Tier2]
        .into_iter()
        .flat_map(|tier| [ListIndentType::Spaces, ListIndentType::Tabs].map(move |indent| (tier, indent)))
}

#[test]
fn should_escape_a_bare_list_marker_before_a_block() {
    for (marker, escaped) in [("-", r"\-"), ("*", r"\*"), ("+", r"\+"), ("1.", r"1\.")] {
        for (html, expected) in [
            (
                format!("<p>{marker}</p><blockquote>q</blockquote>"),
                format!("{escaped}\n> q\n"),
            ),
            (
                format!("{marker}<blockquote>q</blockquote>"),
                format!("{escaped}\n\n> q\n"),
            ),
        ] {
            for (tier, indent) in option_matrix() {
                assert_eq!(
                    convert_with(&html, tier, indent),
                    expected,
                    "marker={marker:?}, tier={tier:?}, indent={indent:?}, html={html:?}"
                );
            }
        }
    }
}

#[test]
fn should_preserve_real_list_items() {
    for (html, expected) in [("<ul><li>q</li></ul>", "- q\n"), ("<ol><li>q</li></ol>", "1. q\n")] {
        for (tier, indent) in option_matrix() {
            assert_eq!(
                convert_with(html, tier, indent),
                expected,
                "tier={tier:?}, indent={indent:?}, html={html:?}"
            );
        }
    }
}
