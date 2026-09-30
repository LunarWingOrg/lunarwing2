// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]

fn convert(html: &str) -> String {
    html_to_markdown_rs::convert(html, None)
        .map(|r| r.content.unwrap_or_default())
        .expect("conversion should succeed")
}

#[test]
fn test_h1_inside_header() {
    let html = "<header><h1>Title in header not exported???</h1></header>";
    let result = convert(html);
    assert_eq!(result, "# Title in header not exported???\n");
}

#[test]
fn test_paragraph_inside_header() {
    let html = "<header><p>Intro text</p></header>";
    let result = convert(html);
    assert_eq!(result, "Intro text\n");
}

#[test]
fn test_header_with_nested_elements() {
    let html = "<header><h1>Title</h1><p>Subtitle</p></header>";
    let result = convert(html);
    assert!(result.contains("# Title"), "Should contain h1: {result}");
    assert!(result.contains("Subtitle"), "Should contain paragraph: {result}");
}

#[test]
fn test_paragraph_inside_footer() {
    let html = "<footer><p>Footer content</p></footer>";
    let result = convert(html);
    assert_eq!(result, "Footer content\n");
}

#[test]
fn test_h2_inside_main() {
    let html = "<main><h2>Main heading</h2></main>";
    let result = convert(html);
    assert_eq!(result, "## Main heading\n");
}

#[test]
fn test_article_with_header_and_section() {
    let html = "<article><header><h1>Title</h1></header><section><p>Content here</p></section></article>";
    let result = convert(html);
    assert!(result.contains("# Title"), "Should contain heading: {result}");
    assert!(result.contains("Content here"), "Should contain content: {result}");
}

#[test]
fn test_heading_inside_section() {
    let html = "<section><h2>Section Heading</h2><p>Section body</p></section>";
    let result = convert(html);
    assert!(result.contains("## Section Heading"), "Should contain h2: {result}");
    assert!(result.contains("Section body"), "Should contain body: {result}");
}

#[test]
fn test_nav_dropped_by_default() {
    let html = r#"<nav><a href="/home">Home</a><a href="/about">About</a></nav>"#;
    let result = convert(html);
    assert!(result.is_empty(), "nav should be dropped by default: '{result}'");
}

#[test]
fn test_nav_preserved_when_remove_navigation_disabled() {
    use html_to_markdown_rs::{ConversionOptions, PreprocessingOptions};
    let opts = ConversionOptions {
        preprocessing: PreprocessingOptions {
            remove_navigation: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let html = r#"<nav><a href="/home">Home</a></nav>"#;
    let result = html_to_markdown_rs::convert(html, Some(opts))
        .map(|r| r.content.unwrap_or_default())
        .expect("conversion should succeed");
    assert!(
        result.contains("Home"),
        "nav should pass through when remove_navigation=false: '{result}'"
    );
}

#[test]
fn test_paragraph_inside_aside() {
    let html = "<aside><p>Side note</p></aside>";
    let result = convert(html);
    assert_eq!(result, "Side note\n");
}

#[test]
fn test_site_chrome_header_dropped() {
    let html = r#"<header class="site-header"><a href="/">Logo</a></header><p>Content</p>"#;
    let result = convert(html);
    assert!(
        !result.contains("Logo"),
        "site-chrome header should be dropped: '{result}'"
    );
    assert!(
        result.contains("Content"),
        "body content should be preserved: '{result}'"
    );
}

#[test]
fn test_header_with_role_navigation_dropped() {
    let html = r#"<header role="navigation"><a href="/">Home</a></header><p>Body</p>"#;
    let result = convert(html);
    assert!(
        !result.contains("Home"),
        "navigation header should be dropped: '{result}'"
    );
    assert!(result.contains("Body"), "body content should be preserved: '{result}'");
}
