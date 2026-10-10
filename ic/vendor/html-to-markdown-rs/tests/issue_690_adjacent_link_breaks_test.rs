#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, OutputFormat, TierStrategy, convert};

fn convert_with(html: &str, output_format: OutputFormat, tier_strategy: TierStrategy) -> String {
    convert(
        html,
        Some(ConversionOptions {
            extract_metadata: false,
            output_format,
            tier_strategy,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion must succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_keep_two_adjacent_breaks_inside_one_link() {
    let html = r#"<ul><li><a href="u">a<br><br>b</a></li></ul>"#;
    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2] {
        let markdown = convert_with(html, OutputFormat::Markdown, tier_strategy);
        let rendered = comrak::markdown_to_html(&markdown, &comrak::Options::default());
        assert_eq!(
            rendered.matches("<a href=\"u\">").count(),
            1,
            "{tier_strategy:?}: {markdown:?}"
        );
        assert_eq!(rendered.matches("<br />").count(), 2, "{tier_strategy:?}: {markdown:?}");

        let djot = convert_with(html, OutputFormat::Djot, tier_strategy);
        assert_eq!(djot.matches("](u)").count(), 1, "{tier_strategy:?}: {djot:?}");
        assert_eq!(djot.matches("\\\n").count(), 2, "{tier_strategy:?}: {djot:?}");
    }
}
