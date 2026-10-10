#![allow(missing_docs)]

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

fn assert_tier2_and_auto(html: &str, expected: &str) {
    let actual = [
        convert_with(html, TierStrategy::Tier2),
        convert_with(html, TierStrategy::Auto),
    ];
    let expected = [expected.to_owned(), expected.to_owned()];
    assert_eq!(actual, expected, "Tier2 and Auto: {html}");
}

#[test]
fn should_not_invent_spaces_around_an_abbreviation_in_a_link_in_a_layout_cell() {
    let html = r#"<table border="0"><tr><td><a href="u">Realm (<abbr title="virology">vir.</abbr>)</a></td><td colspan="1">x</td></tr></table>"#;
    assert_tier2_and_auto(html, "- [Realm (vir. (virology))](u) x\n");
}

#[test]
fn should_not_invent_spaces_between_strong_text_and_a_slash_in_a_link_in_a_layout_cell() {
    let html = r#"<table border="0"><tr><td><a href="u"><strong>Domain</strong>/Superkingdom</a></td><td colspan="1">x</td></tr></table>"#;
    assert_tier2_and_auto(html, "- [**Domain**/Superkingdom](u) x\n");
}

#[test]
fn should_not_invent_spaces_between_adjacent_images_in_a_link_in_a_layout_cell() {
    let html = r#"<table border="0"><tr><td><a href="u"><img src="a.png" alt="a"><img src="b.png" alt="b"></a></td><td colspan="1">x</td></tr></table>"#;
    assert_tier2_and_auto(html, "- [![a](a.png)![b](b.png)](u) x\n");
}
