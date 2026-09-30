//! Tier-1 alt/title entity handling (Phase DD).
//!
//! Tier-2 runs source HTML through html5ever for repair when custom
//! elements are present.  html5ever decodes numeric entities like
//! `&#x22;` and re-emits them in the canonical named form `&quot;`.
//! Without custom elements, Tier-2 reads tl's raw attribute bytes
//! verbatim — entities are preserved unchanged.
//!
//! Tier-1 has no html5ever roundtrip, so:
//!   * Without custom elements: pass tl's raw bytes through.
//!   * With custom elements: canonicalize entities to match.

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

fn assert_matches(html: &str) {
    let t1 = tier1(html);
    let t2 = tier2(html);
    assert_eq!(
        t1, t2,
        "tier1 diverged from tier2\ninput: {html:?}\ntier1: {t1:?}\ntier2: {t2:?}"
    );
}

// ~keep ── No custom elements: T2 preserves entities verbatim. ──────────────────────

#[test]
fn plain_named_quote_preserved_verbatim() {
    assert_matches(r#"<p><img src="/x.png" alt="hello &quot;world&quot;"></p>"#);
}

#[test]
fn plain_amp_preserved_verbatim() {
    assert_matches(r#"<p><img src="/x.png" alt="A &amp; B"></p>"#);
}

#[test]
fn plain_hex_entity_preserved_verbatim() {
    assert_matches(r#"<p><img src="/x.png" alt="hello &#x22;w&#x22;"></p>"#);
}

// ~keep ── Custom elements present: T2 canonicalizes via html5ever roundtrip. ───────

#[test]
fn with_custom_element_hex_entity_canonicalized() {
    assert_matches(r#"<my-component>x</my-component><p><img src="/x.png" alt="hello &#x22;w&#x22;"></p>"#);
}

#[test]
fn with_custom_element_amp_canonicalized() {
    assert_matches(r#"<my-component>x</my-component><p><img src="/x.png" alt="A &amp; B"></p>"#);
}

#[test]
fn with_custom_element_title_canonicalized() {
    assert_matches(r#"<my-component>x</my-component><p><img src="/x.png" alt="a" title="&#x22;t&#x22;"></p>"#);
}
