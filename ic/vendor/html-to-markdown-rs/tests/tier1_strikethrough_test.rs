//! Tier-1 strikethrough (Phase Z).
//!
//! Tier-2 emits `~~content~~` for `<del>`, `<s>`, and `<strike>`.
//! Tier-1 used to map them to transparent inline, dropping the wrap.

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

#[test]
fn del_emits_tildes() {
    assert!(tier1("<p><del>x</del></p>").contains("~~x~~"));
}

#[test]
fn s_emits_tildes() {
    assert!(tier1("<p><s>x</s></p>").contains("~~x~~"));
}

#[test]
fn strike_emits_tildes() {
    assert!(tier1("<p><strike>x</strike></p>").contains("~~x~~"));
}

#[test]
fn del_inside_code_no_tildes() {
    // ~keep Tier-2's handle_strikethrough suppresses wrapping inside <code>.
    let out = tier1("<p><code>raw <del>x</del> y</code></p>");
    assert!(!out.contains("~~"), "no tildes inside code: {out:?}");
}
