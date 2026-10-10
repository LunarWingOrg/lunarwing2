#![allow(missing_docs)]

use html_to_markdown_rs::{ConversionOptions, OutputFormat, convert};

fn djot(html: &str) -> String {
    convert(
        html,
        Some(ConversionOptions {
            output_format: OutputFormat::Djot,
            ..ConversionOptions::default()
        }),
    )
    .expect("conversion should succeed")
    .content
    .unwrap_or_default()
}

#[test]
fn should_escape_literal_stars_in_thematic_break_shaped_djot_text() {
    for (html, expected) in [
        ("<p>* - *</p>", "\\* - \\*\n"),
        ("<p>***</p>", "\\*\\*\\*\n"),
        ("<p>* * *</p>", "\\* \\* \\*\n"),
        ("<p>*-*</p>", "\\*-\\*\n"),
        ("<p>*</p>", "\\*\n"),
    ] {
        assert_eq!(djot(html), expected, "{html}");
    }
}

#[test]
fn should_escape_literal_stars_outside_explicit_paragraphs() {
    for html in [
        "* - *",
        "<div>* - *</div>",
        "<span>* - *</span>",
        "<section>* - *</section>",
    ] {
        assert_eq!(djot(html), "\\* - \\*\n", "{html}");
    }
}

#[test]
fn should_classify_rule_like_text_from_the_complete_container_text() {
    for html in [
        "<p>* <span>hello</span></p>",
        "<div>* <span>hello</span></div>",
        "<span>* <span>hello</span></span>",
        "<section>* <span>hello</span></section>",
    ] {
        assert_eq!(djot(html), "* hello\n", "{html}");
    }

    assert_eq!(djot("<p>*<b>hello</b></p>"), "**hello*\n");
}

#[test]
fn should_escape_segmented_rule_like_text_in_every_container() {
    for html in [
        "<p>* <span>-</span> *</p>",
        "<div>* <span>-</span> *</div>",
        "<span>* <span>-</span> *</span>",
        "<section>* <span>-</span> *</section>",
    ] {
        assert_eq!(djot(html), "\\* - \\*\n", "{html}");
    }
}

#[test]
fn should_classify_each_logical_line_across_inline_descendants() {
    assert_eq!(djot("<p>* - *<br>hello</p>"), "\\* - \\*\\\nhello\n");
    assert_eq!(djot("<p>* <span>-</span> *<br>hello</p>"), "\\* - \\*\\\nhello\n");
}

#[test]
fn should_escape_only_literal_stars_around_djot_strong_markup() {
    assert_eq!(djot("<p>*<b>-</b>*</p>"), "\\**-*\\*\n");
    assert_eq!(djot("<p><b>- *</b></p>"), "*- \\**\n");
}

#[test]
fn should_not_escape_stars_in_ordinary_djot_text() {
    assert_eq!(djot("* hello"), "* hello\n");
    assert_eq!(djot("<p>2 * 3</p>"), "2 * 3\n");
    assert_eq!(djot("<p>* hello</p>"), "* hello\n");
}
