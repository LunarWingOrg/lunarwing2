#![allow(missing_docs)]

use html_to_markdown_rs::{convert, options::ConversionOptions};

fn content(html: &str) -> String {
    convert(html, Some(ConversionOptions::default()))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_preserve_visible_images_inside_zero_font_size_containers() {
    for html in [
        "<div style='font-size:0px'><img src='banner.png' width='600'></div>",
        "<img style='font-size:0px' src='banner.png' width='600'>",
    ] {
        assert_eq!(content(html), "![](banner.png)\n", "input: {html}");
    }
}

#[test]
fn should_preserve_images_inside_zero_font_size_table_cells() {
    let html = "<table><tr><td style='font-size:0px'><img src='banner.png' width='600'></td></tr></table>";
    let expected = content("<table><tr><td><img src='banner.png' width='600'></td></tr></table>");
    assert!(expected.contains("![](banner.png)"));
    assert_eq!(content(html), expected);
}

#[test]
fn should_still_remove_definitively_hidden_images() {
    for html in [
        "<div style='font-size:0;display:none'><img src='banner.png' width='600'></div>",
        "<div hidden style='font-size:0'><img src='banner.png' width='600'></div>",
        "<div style='font-size:0'>secret<img hidden src='banner.png' width='600'></div>",
        "<div style='font-size:0'>secret<span hidden><img src='banner.png'></span></div>",
        "<div style='font-size:0'>secret<!-- <img src='banner.png'> --></div>",
    ] {
        assert_eq!(content(html), "", "input: {html}");
    }
}

#[test]
fn should_preserve_following_table_cell_after_misnested_office_paragraphs() {
    let malformed =
        "<TABLE><TR><TD><o:p><P></o:p></P></TD><TD><IMG src=\"image.png\">Visible text</TD></TR></TABLE><P></P>";
    let repaired =
        "<TABLE><TR><TD><o:p><P></P></o:p></TD><TD><IMG src=\"image.png\">Visible text</TD></TR></TABLE><P></P>";
    let expected = content(repaired);
    assert!(expected.contains("Visible text"));
    assert!(expected.contains("![](image.png)"));
    assert_eq!(content(malformed), expected);
}

#[cfg(feature = "metadata")]
#[test]
fn should_preserve_valid_office_document_head_metadata() {
    let html = "<html><head><o:p></o:p><meta name='Generator' content='Word'><title>Document</title></head><body><p>Visible</p></body></html>";
    assert_eq!(
        content(html),
        "---\nmeta-Generator: Word\ntitle: Document\n---\n\nVisible\n"
    );
}
