//! Tier-1 inter-tag whitespace preservation (Phase U).
//!
//! When a whitespace-only text node appears between two adjacent inline
//! elements (e.g. `</strong> <em>`), Tier-1 must preserve a single space
//! rather than dropping the whitespace entirely.  Tier-2 preserves it
//! because the DOM walker visits the text node inside the block container;
//! Tier-1's heuristic used to drop it when no inline frame was on the stack.

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

/// `</strong> <a>` — the most common real-world case (kimbrain-class HTML).
#[test]
fn inter_tag_whitespace_strong_then_link() {
    let html = r#"<p><strong>a</strong> <a href="x">b</a></p>"#;
    let t1 = tier1(html);
    assert!(t1.contains("**a** [b](x)"), "space missing: tier1={t1:?}");
}

/// `</em> <strong>` — emphasis then strong.
#[test]
fn inter_tag_whitespace_em_then_strong() {
    let html = "<p><em>a</em> <strong>b</strong></p>";
    let t1 = tier1(html);
    assert!(t1.contains("*a* **b**"), "space missing: tier1={t1:?}");
}

/// `</a> <strong>` — link close then strong.
#[test]
fn inter_tag_whitespace_link_then_strong() {
    let html = r#"<p><a href="x">a</a> <strong>b</strong></p>"#;
    let t1 = tier1(html);
    assert!(t1.contains("[a](x) **b**"), "space missing: tier1={t1:?}");
}

/// `</code> <strong>` — code close then strong.
#[test]
fn inter_tag_whitespace_code_then_strong() {
    let html = "<p><code>a</code> <strong>b</strong></p>";
    let t1 = tier1(html);
    assert!(t1.contains("`a` **b**"), "space missing: tier1={t1:?}");
}

/// Multiple spaces between tags collapse to a single space.
#[test]
fn inter_tag_whitespace_multiple_spaces_collapse() {
    let html = "<p><strong>a</strong>   <em>b</em></p>";
    let t1 = tier1(html);
    assert!(t1.contains("**a** *b*"), "space missing or doubled: tier1={t1:?}");
    assert!(!t1.contains("**a**  *b*"), "double-space emitted: tier1={t1:?}");
}

/// `</strong> <em>` at the top level — no paragraph wrapper.
#[test]
fn inter_tag_whitespace_strong_then_em_in_paragraph() {
    let html = "<p><strong>bold</strong> <em>italic</em></p>";
    let t1 = tier1(html);
    assert!(t1.contains("**bold** *italic*"), "space missing: tier1={t1:?}");
}

/// Whitespace between block-level siblings must not become a space.
#[test]
fn inter_tag_whitespace_between_paragraphs_not_preserved() {
    let html = "<p>a</p>\n<p>b</p>";
    let t1 = tier1(html);
    assert!(t1.contains("a\n\nb"), "expected blank-line separator, got {t1:?}");
    assert!(!t1.contains("a b"), "unexpected space between paragraphs: {t1:?}");
}

/// Leading whitespace inside a block element (before first inline) must not
/// gain an extra space from the fix.  Tier-1 already trims structural leading
/// whitespace; verify it stays trimmed and does NOT start with a space char
/// introduced by the inter-tag fix.
#[test]
fn inter_tag_whitespace_leading_inside_block_trimmed() {
    let html = "<div>  <strong>x</strong></div>";
    let t1 = tier1(html);
    // ~keep Tier-1 trims the leading `  ` (existing behaviour).  The fix must not
    // ~keep add a spurious leading space here (output tail before <strong> is `\n\n`
    // ~keep or empty — a block edge, not an inline-close marker).
    assert!(!t1.starts_with(' '), "unexpected leading space: {t1:?}");
}

/// Whitespace between block-level divs must not become an inter-tag space.
#[test]
fn inter_tag_whitespace_between_divs_not_preserved() {
    let html = "<div>foo</div>  <div>bar</div>";
    let t1 = tier1(html);
    assert!(!t1.contains("  "), "double-space leaked: {t1:?}");
}
