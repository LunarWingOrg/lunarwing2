// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]

use html_to_markdown_rs::convert;

fn markdown(html: &str) -> String {
    convert(html, None)
        .expect("conversion should succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_keep_sectioning_container_blocks_inside_a_list_item() {
    for tag in ["div", "section", "article", "aside", "header", "footer", "main"] {
        let html = format!("<ul><li><{tag}><p>One</p><p>Two</p></{tag}></li></ul>");
        assert_eq!(markdown(&html), "- One\n\n  Two\n", "tag: {tag}");
    }
}

#[test]
fn should_keep_a_heading_after_introductory_text_inside_a_list_item() {
    let html = "<ul><li><p>Intro</p><h2>Question?</h2><p>Answer</p></li></ul>";
    assert_eq!(markdown(html), "- Intro\n\n  ## Question?\n\n  Answer\n");
}

#[test]
fn should_not_turn_section_content_after_a_heading_into_an_indented_code_block() {
    let html = "<ul><li><section><p>Intro</p><h2>Question?</h2><div><p>Answer</p></div></section></li></ul>";
    assert_eq!(markdown(html), "- Intro\n\n  ## Question?\n\n  Answer\n");
}
