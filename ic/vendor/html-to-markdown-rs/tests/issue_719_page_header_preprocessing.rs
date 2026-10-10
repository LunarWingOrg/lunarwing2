// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]
#![cfg(feature = "testkit")]

use html_to_markdown_rs::{
    ConversionOptions, OutputFormat, PreprocessingOptions, PreprocessingPreset, TierStrategy, convert,
};

fn convert_with(html: &str, preset: PreprocessingPreset, tier_strategy: TierStrategy) -> String {
    let options = ConversionOptions {
        extract_metadata: false,
        preprocessing: PreprocessingOptions {
            enabled: true,
            preset,
            ..PreprocessingOptions::default()
        },
        tier_strategy,
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_remove_a_page_header_with_every_cleanup_preset_that_removes_navigation() {
    for html in [
        "<body><header><a href=\"/\">Home</a></header><nav><a href=\"/menu\">Menu</a></nav><main><p>Content</p></main></body>",
        "<body><div><header><a href=\"/\">Home</a></header></div><main><p>Content</p></main></body>",
    ] {
        for preset in [PreprocessingPreset::Standard, PreprocessingPreset::Aggressive] {
            assert_eq!(
                convert_with(html, preset, TierStrategy::Auto),
                "Content\n",
                "{preset:?}"
            );
            assert_eq!(
                convert_with(html, preset, TierStrategy::Tier2),
                "Content\n",
                "{preset:?}"
            );
        }
    }
}

#[test]
fn should_keep_headers_that_belong_to_article_content() {
    for container in ["article", "section", "main"] {
        let html = format!("<{container}><header><h2>Title</h2></header><p>Content</p></{container}>");
        for preset in [PreprocessingPreset::Standard, PreprocessingPreset::Aggressive] {
            assert_eq!(
                convert_with(&html, preset, TierStrategy::Tier2),
                "## Title\n\nContent\n",
                "container: {container}, preset: {preset:?}"
            );
        }
    }
}

#[test]
fn should_remove_a_page_header_in_the_fast_converter() {
    let html = "<body><header><a href=\"/\">Home</a></header><main><p>Content</p></main></body>";
    assert_eq!(
        convert_with(html, PreprocessingPreset::Standard, TierStrategy::Tier1),
        "Content\n"
    );
}

#[test]
fn should_remove_a_header_only_navigation_document() {
    let html = "<body><div><header><a href=\"/en\">English</a><a href=\"/de\">Deutsch</a></header></div></body>";
    for tier_strategy in [TierStrategy::Tier1, TierStrategy::Tier2] {
        assert_eq!(convert_with(html, PreprocessingPreset::Standard, tier_strategy), "");
    }

    let options = ConversionOptions {
        extract_metadata: false,
        output_format: OutputFormat::Plain,
        preprocessing: PreprocessingOptions {
            enabled: true,
            ..PreprocessingOptions::default()
        },
        ..ConversionOptions::default()
    };
    assert_eq!(
        convert(html, Some(options))
            .expect("conversion should succeed")
            .content
            .unwrap_or_default(),
        ""
    );
}

#[test]
fn should_remove_a_page_header_from_plain_text_output() {
    let html = "<body><header><a href=\"/\">Home</a></header><main><p>Content</p></main></body>";
    let options = ConversionOptions {
        extract_metadata: false,
        output_format: OutputFormat::Plain,
        preprocessing: PreprocessingOptions {
            enabled: true,
            ..PreprocessingOptions::default()
        },
        ..ConversionOptions::default()
    };
    let output = convert(html, Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default();
    assert_eq!(output, "Content\n");
}
