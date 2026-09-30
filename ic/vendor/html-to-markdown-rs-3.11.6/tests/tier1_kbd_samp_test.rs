//! Tier-1 `<kbd>` / `<samp>` rendered as code spans (Phase W).
//!
//! Tier-2 wraps `<kbd>`, `<samp>`, and `<code>` content in backticks via the
//! shared inline/code.rs path.  Tier-1 used to map `<kbd>` and `<samp>` to
//! plain inline (transparent), dropping the backticks.  This file pins the
//! corrected behaviour.

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
fn kbd_emits_backticks() {
    assert!(tier1("<p>press <kbd>#</kbd> now</p>").contains("press `#` now"));
}

#[test]
fn samp_emits_backticks() {
    assert!(tier1("<p>output: <samp>OK</samp></p>").contains("output: `OK`"));
}

#[test]
fn kbd_inside_paragraph_with_surrounding_text() {
    let out = tier1("<p>press <kbd>#</kbd> now</p>");
    assert!(out.contains("press `#` now"), "got: {out:?}");
}
