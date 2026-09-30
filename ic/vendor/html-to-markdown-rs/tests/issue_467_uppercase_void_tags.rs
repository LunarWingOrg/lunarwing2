//! HTML tag names are case-insensitive, but the vendored `astral-tl` parser compares a tag's
//! raw source bytes against an all-lowercase void-element table
//! (`astral-tl-0.8.0/src/parser/base.rs`). An uppercase void element therefore misses the
//! table and is pushed onto the open-element stack as a *container*, and the matching close
//! tag of its real parent cannot pop it — so every following sibling is absorbed as its child.
//!
//! Reported as issue #467 with `<META>` in `<head>`: the body became a grandchild of `<head>`,
//! which put it out of reach of `handle_head`'s direct-child rescue in `converter/metadata.rs`
//! and made `convert()` return an empty string. The `charset` attribute in the report is a red
//! herring — the crate has no charset handling at all, and every void element reproduces it.
//!
//! Tier-1 lowercases tag names before lookup and was always correct here, so each case is
//! asserted across both tiers: the library picks a tier automatically, and these inputs are
//! exactly the shape that made one document convert two ways.

#![cfg(feature = "testkit")]
#![allow(missing_docs)]

use html_to_markdown_rs::prescan::PrescanReport;
use html_to_markdown_rs::tier1;
use html_to_markdown_rs::{ConversionOptions, HighlightStyle, TierStrategy, convert};

fn tier1_friendly_options() -> ConversionOptions {
    ConversionOptions {
        extract_metadata: false,
        highlight_style: HighlightStyle::None,
        ..ConversionOptions::default()
    }
}

/// Runs the Tier-1 scanner directly (never through `convert()`'s fallback dispatch) so a
/// bail is a hard test failure, not a silent fall-through to Tier-2.
fn run_tier1(html: &str) -> String {
    let report = PrescanReport::default();
    match tier1::run(html, &report, &tier1_friendly_options()) {
        Ok(markdown) => markdown,
        Err(reason) => panic!("tier1 bailed on {html:?}: {reason:?}"),
    }
}

fn run_tier2(html: &str) -> String {
    let mut options = tier1_friendly_options();
    options.tier_strategy = TierStrategy::Tier2;
    convert(html, Some(options)).unwrap().content.unwrap_or_default()
}

fn assert_both_tiers(html: &str, expected: &str) {
    let t2 = run_tier2(html);
    assert_eq!(t2, expected, "tier2 output changed for {html:?}");
    let t1 = run_tier1(html);
    assert_eq!(
        t1, t2,
        "tier1 diverged from tier2\ninput: {html:?}\ntier1: {t1:?}\ntier2: {t2:?}"
    );
}

/// The exact reproduction from issue #467, with default options (which force Tier-2 because
/// `extract_metadata` defaults to true). Before the fix this returned `""`.
#[test]
fn should_keep_the_body_when_head_holds_an_uppercase_meta() {
    let html = concat!(
        r#"<html><head><META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=us-ascii">"#,
        "</head><body><p>Signature placeholder</p></body></html>"
    );
    let content = convert(html, None).unwrap().content.unwrap_or_default();
    assert_eq!(content, "Signature placeholder\n", "actual: {content:?}");
}

/// The control from the issue: identical but for the tag name's case. Both spellings must
/// produce the same visible content.
#[test]
fn should_convert_uppercase_and_lowercase_meta_identically() {
    let upper = concat!(
        r#"<html><head><META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=us-ascii">"#,
        "</head><body><p>Signature placeholder</p></body></html>"
    );
    let lower = upper.replacen("<META", "<meta", 1);
    assert_ne!(upper, lower, "the two inputs must actually differ");

    let from_upper = convert(upper, None).unwrap().content.unwrap_or_default();
    let from_lower = convert(lower.as_str(), None).unwrap().content.unwrap_or_default();
    assert_eq!(from_upper, from_lower, "tag-name case changed the output");
}

/// `<META>` is not special: every HTML5 void element hit the same parser path, swallowing
/// whatever followed it inside the same parent.
#[test]
fn should_not_swallow_siblings_after_any_uppercase_void_element() {
    for void_tag in ["<BR>", "<HR>", "<IMG>", "<INPUT>", "<LINK>", "<WBR>", "<Br>", "<hR>"] {
        let html = format!("<div><span>before</span>{void_tag}<span>after</span></div>");
        let content = convert(html.as_str(), None).unwrap().content.unwrap_or_default();
        assert!(
            content.contains("before") && content.contains("after"),
            "{void_tag} swallowed its following sibling: {content:?}"
        );
    }
}

/// Tier-1 always handled this correctly, so the fix must close the divergence rather than
/// move it — both tiers have to agree byte for byte.
#[test]
fn should_agree_across_both_tiers_on_uppercase_void_elements() {
    for (upper, lower) in [
        ("<p>one<BR>two</p>", "<p>one<br>two</p>"),
        ("<p>a</p><HR><p>b</p>", "<p>a</p><hr><p>b</p>"),
    ] {
        let expected = run_tier2(lower);
        assert_both_tiers(upper, expected.as_str());
    }
}

/// The rewrite must touch the tag name only. An uppercase attribute name, and a value whose
/// bytes spell a void element, must both survive untouched.
#[test]
fn should_lowercase_only_the_tag_name_and_leave_attributes_alone() {
    let upper = convert(r#"<p><IMG SRC="META.png" ALT="A BR IMG"></p>"#, None)
        .unwrap()
        .content
        .unwrap_or_default();
    let lower = convert(r#"<p><img SRC="META.png" ALT="A BR IMG"></p>"#, None)
        .unwrap()
        .content
        .unwrap_or_default();
    assert_eq!(upper, lower, "tag-name case changed the output");
    assert!(upper.contains("META.png"), "attribute value was rewritten: {upper:?}");
    assert!(upper.contains("A BR IMG"), "attribute value was rewritten: {upper:?}");
}
