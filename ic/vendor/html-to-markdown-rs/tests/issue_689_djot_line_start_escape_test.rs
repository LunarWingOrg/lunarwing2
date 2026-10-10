#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, OutputFormat, TierStrategy, convert};

fn convert_djot(html: &str, tier_strategy: TierStrategy) -> String {
    convert(
        html,
        Some(ConversionOptions {
            extract_metadata: false,
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
fn should_escape_each_djot_dash_and_backtick_at_a_line_start_after_a_break() {
    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2] {
        assert_eq!(
            convert_djot("<p>a<br><span>---</span></p>", tier_strategy),
            "a\\\n\\-\\-\\-\n"
        );
        assert_eq!(
            convert_djot("<p>a<br><span>-</span><span>--</span></p>", tier_strategy),
            "a\\\n\\-\\-\\-\n"
        );
        assert_eq!(convert_djot("<p>a<br>-</p>", tier_strategy), "a\\\n-\n");
        assert_eq!(convert_djot("<p>a<br>`</p>", tier_strategy), "a\\\n\\`\n");
    }

    let html = r#"<ol start="10"><li><a href="u">a<br><span>```</span></a></li></ol>"#;
    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2] {
        assert_eq!(convert_djot(html, tier_strategy), "10. [a\\\n    \\`\\`\\`](u)\n");
    }
}

#[test]
fn should_escape_djot_marker_runs_after_a_break_inside_detached_inline_wrappers() {
    for (tag, marker) in [("abbr", ""), ("sub", "~"), ("sup", "^"), ("label", "")] {
        for (text, escaped) in [("---", r"\-\-\-"), ("```", r"\`\`\`")] {
            let html = format!("<p>a<br><{tag}>{text}</{tag}></p>");
            assert_eq!(
                convert_djot(&html, TierStrategy::Tier2),
                format!("a\\\n{marker}{escaped}{marker}\n"),
                "input: {html:?}"
            );
        }
    }
}
