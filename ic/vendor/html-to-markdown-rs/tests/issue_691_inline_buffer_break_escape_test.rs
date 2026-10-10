#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn convert_with(html: &str, tier_strategy: TierStrategy) -> String {
    convert(
        html,
        Some(ConversionOptions {
            extract_metadata: false,
            tier_strategy,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion must succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_escape_block_openers_in_detached_inline_buffers_after_a_break() {
    for tag in ["abbr", "sub", "sup", "label"] {
        for (text, escaped) in [
            ("- t", "\\- t"),
            ("1) t", "1\\) t"),
            ("# t", "\\# t"),
            ("---", "\\---"),
            ("> t", "\\> t"),
        ] {
            let html = format!("<p>a<br><{tag}>{text}</{tag}></p>");
            let expected = format!("a  \n{escaped}\n");
            for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2] {
                assert_eq!(
                    convert_with(&html, tier_strategy),
                    expected,
                    "{tier_strategy:?}: {html}"
                );
            }
        }
    }
}
