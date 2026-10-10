#![allow(missing_docs)]

//! Regression test for issue #643: a block in an inline wrapper must leave the checkbox line.

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

#[test]
fn should_start_an_inline_wrapped_quote_below_the_task_checkbox() {
    let options = ConversionOptions {
        extract_metadata: false,
        tier_strategy: TierStrategy::Tier2,
        ..ConversionOptions::default()
    };

    for (tag, marker, rendered_body) in [
        ("b", "**", "<strong>q</strong>"),
        ("strong", "**", "<strong>q</strong>"),
        ("em", "*", "<em>q</em>"),
        ("i", "*", "<em>q</em>"),
        ("del", "~~", "<del>q</del>"),
        ("s", "~~", "<del>q</del>"),
        ("strike", "~~", "<del>q</del>"),
    ] {
        let html = format!(r#"<ul><li><input type="checkbox"><{tag}><blockquote>q</blockquote></{tag}></li></ul>"#);
        let markdown = convert(&html, Some(options.clone()))
            .expect("conversion must succeed")
            .content
            .unwrap_or_default();

        assert_eq!(markdown, format!("- [ ] &#32;\n  > {marker}q{marker}\n"), "{tag}");
        let mut render_options = comrak::Options::default();
        render_options.extension.strikethrough = true;
        let rendered = comrak::markdown_to_html(&markdown, &render_options);
        assert!(
            rendered.contains(&format!("<blockquote>\n<p>{rendered_body}</p>\n</blockquote>")),
            "{tag}: {rendered}"
        );
    }
}
