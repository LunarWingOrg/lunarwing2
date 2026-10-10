// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, OutputFormat, TierStrategy, convert};

fn djot(html: &str, tier_strategy: TierStrategy) -> String {
    convert(
        html,
        Some(ConversionOptions {
            extract_metadata: false,
            output_format: OutputFormat::Djot,
            tier_strategy,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion should succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_write_djot_table_separator_without_spaces() {
    let html = "<table><tr><th>h</th></tr><tr><td>x</td></tr></table>";
    let expected = "| h |\n|---|\n| x |\n";

    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2, TierStrategy::Auto] {
        assert_eq!(djot(html, tier_strategy), expected, "{tier_strategy:?}");
    }
}

#[test]
fn should_escape_literal_dashes_in_djot_table_cells() {
    let html = "<table><tr><th>h</th></tr><tr><td>a---b</td></tr></table>";

    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2, TierStrategy::Auto] {
        let output = djot(html, tier_strategy);
        assert!(output.contains(r"a\-\-\-b"), "{tier_strategy:?}: {output:?}");
        assert!(!output.contains("a---b"), "{tier_strategy:?}: {output:?}");
    }
}
