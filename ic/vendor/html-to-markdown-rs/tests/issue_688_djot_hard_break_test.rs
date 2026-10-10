#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, NewlineStyle, OutputFormat, TierStrategy, convert};

fn convert_djot(html: &str, newline_style: NewlineStyle, tier_strategy: TierStrategy) -> String {
    convert(
        html,
        Some(ConversionOptions {
            extract_metadata: false,
            newline_style,
            output_format: OutputFormat::Djot,
            tier_strategy,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion must succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_write_a_djot_hard_break_with_a_backslash_for_every_newline_style() {
    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2] {
        for newline_style in [NewlineStyle::Spaces, NewlineStyle::Backslash] {
            assert_eq!(convert_djot("<p>a<br>b</p>", newline_style, tier_strategy), "a\\\nb\n");
            assert_eq!(
                convert_djot(r#"<p><a href="u">a<br>b</a></p>"#, newline_style, tier_strategy),
                "[a\\\nb](u)\n"
            );
        }
    }
}

#[test]
fn should_preserve_djot_hard_breaks_at_tier1_link_boundaries() {
    assert_eq!(
        convert_djot(
            r#"<p><a href="H"><br>A</a></p>"#,
            NewlineStyle::Spaces,
            TierStrategy::Tier1,
        ),
        "[\\\nA](H)\n"
    );
    assert_eq!(
        convert_djot(
            r#"<p><a href="H">A<br></a>B</p>"#,
            NewlineStyle::Spaces,
            TierStrategy::Tier1,
        ),
        "[A\\\n](H)B\n"
    );
}
