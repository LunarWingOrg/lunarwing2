//! Tier-1 `<figcaption>` italic-wrap tests (Phase FF-2).
//!
//! Mirrors Tier-2's `semantic/figure.rs::handle_figcaption`: content is
//! collected into a buffer, trimmed, wrapped in `*…*\n\n`.

#![cfg(feature = "testkit")]

use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert};

fn tier1(html: &str) -> String {
    let opts = ConversionOptions {
        tier_strategy: TierStrategy::Tier1,
        extract_metadata: false,
        ..ConversionOptions::default()
    };
    convert(html, Some(opts)).unwrap().content.unwrap_or_default()
}

fn tier2(html: &str) -> String {
    let opts = ConversionOptions {
        tier_strategy: TierStrategy::Tier2,
        extract_metadata: false,
        ..ConversionOptions::default()
    };
    convert(html, Some(opts)).unwrap().content.unwrap_or_default()
}

fn assert_matches_tier2(html: &str) {
    let t1 = tier1(html);
    let t2 = tier2(html);
    assert_eq!(
        t1, t2,
        "tier1 diverged from tier2\ninput: {html:?}\ntier1: {t1:?}\ntier2: {t2:?}"
    );
}

#[test]
fn figcaption_bare_simple_text() {
    assert_matches_tier2("<figcaption>caption</figcaption>");
}

#[test]
fn figcaption_with_inline_link() {
    assert_matches_tier2(r#"<figcaption>see <a href="/x">x</a> for more</figcaption>"#);
}

#[test]
fn figcaption_empty_emits_nothing() {
    assert_matches_tier2("<figcaption></figcaption>");
}

#[test]
fn figcaption_whitespace_only_emits_nothing() {
    assert_matches_tier2("<figcaption>   </figcaption>");
}

#[test]
fn figcaption_inside_figure_with_image() {
    assert_matches_tier2(r#"<figure><img src="x.png" alt="alt"/><figcaption>caption</figcaption></figure>"#);
}

#[test]
fn figcaption_after_paragraph_has_blank_line_separator() {
    assert_matches_tier2("<p>before</p><figcaption>caption</figcaption>");
}

#[test]
fn figcaption_with_emphasis_children() {
    assert_matches_tier2("<figcaption>see <strong>this</strong> note</figcaption>");
}
