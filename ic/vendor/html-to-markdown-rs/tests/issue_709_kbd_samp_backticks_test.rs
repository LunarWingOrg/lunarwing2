#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn convert_with_tier(html: &str, tier_strategy: TierStrategy) -> String {
    convert(
        html,
        Some(ConversionOptions {
            tier_strategy,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion should succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_choose_a_safe_code_span_delimiter_for_kbd_and_samp() {
    for tier in [TierStrategy::Tier1, TierStrategy::Tier2] {
        for tag in ["kbd", "samp"] {
            let html = format!("<p><{tag}>a`b</{tag}></p>");
            assert_eq!(convert_with_tier(&html, tier), "``a`b``\n", "{tier:?}: <{tag}>");
        }
    }
}

#[test]
fn should_use_the_same_code_span_rules_for_code_kbd_and_samp() {
    for tier in [TierStrategy::Tier1, TierStrategy::Tier2] {
        for body in ["a``b", "`edge", "edge`", "a`b``c"] {
            let code = convert_with_tier(&format!("<p><code>{body}</code></p>"), tier);
            for tag in ["kbd", "samp"] {
                let html = format!("<p><{tag}>{body}</{tag}></p>");
                assert_eq!(convert_with_tier(&html, tier), code, "{tier:?}: <{tag}>{body}</{tag}>");
            }
        }
    }
}
