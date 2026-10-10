#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn convert_with(html: &str, tier_strategy: TierStrategy) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        tier_strategy,
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion must succeed")
        .content
        .unwrap_or_default()
}

fn assert_all_tiers(html: &str, expected: &str) {
    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2, TierStrategy::Auto] {
        assert_eq!(
            convert_with(html, tier_strategy),
            expected,
            "{html} ({tier_strategy:?})"
        );
    }
}

#[test]
fn should_escape_text_that_would_start_a_markdown_block() {
    for (html, expected) in [
        ("<p>1. t</p>", "1\\. t\n"),
        ("<p>2) t</p>", "2\\) t\n"),
        ("<p>- t</p>", "\\- t\n"),
        ("<p># t</p>", "\\# t\n"),
        ("<p>&gt; t</p>", "\\> t\n"),
        ("<p>~~~</p>", "\\~~~\n"),
    ] {
        assert_all_tiers(html, expected);
    }
}

#[test]
fn should_escape_a_block_opener_after_a_hard_break() {
    assert_all_tiers("<p>a<br>- t</p>", "a  \n\\- t\n");
}

#[test]
fn should_escape_a_list_marker_at_the_start_of_list_item_text() {
    assert_all_tiers("<ul><li>01.</li></ul>", "- 01\\.\n");
    assert_all_tiers(
        "<ul><li><div>01.</div><h3>Title</h3></li></ul>",
        "- 01\\.\n\n  ### Title\n",
    );
}

#[test]
fn should_treat_task_marker_text_as_paragraph_text_but_escape_nested_item_text() {
    assert_all_tiers(r#"<ul><li><input type="checkbox">&gt; x</li></ul>"#, "- [ ] > x\n");
    assert_all_tiers(
        r#"<blockquote><ul><li><input type="checkbox">&gt; x</li></ul></blockquote>"#,
        "> - [ ] > x\n",
    );
    assert_all_tiers(
        r#"<ul><li><input type="checkbox"><ul><li>&gt; x</li></ul></li></ul>"#,
        "- [ ] &#32;\n  * \\> x\n",
    );
    assert_all_tiers(
        r#"<ul><li><input type="checkbox"><blockquote>&gt; x</blockquote></li></ul>"#,
        "- [ ] &#32;\n  > \\> x\n",
    );
}

#[test]
fn should_not_escape_markdown_characters_in_running_text() {
    assert_all_tiers("<p>1. version 3.5 of C++</p>", "1\\. version 3.5 of C++\n");
    assert_all_tiers("<h2>1. t</h2>", "## 1. t\n");
    assert_all_tiers("<p><strong>1. t</strong></p>", "**1. t**\n");
    assert_all_tiers("<p>*<em>foo</em></p>", "**foo*\n");
    assert_all_tiers("<p>*<strong>foo</strong></p>", "***foo**\n");
}
