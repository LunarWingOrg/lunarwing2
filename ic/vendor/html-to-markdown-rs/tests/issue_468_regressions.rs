// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]

//! Regression tests for issue #468: `font-size: 0` joins `display: none`, `visibility: hidden`
//! and the `hidden` attribute as a declaration that keeps an element out of the output.
//!
//! Reported against a generated email banner, where a `font-size:0` wrapper leaked its marker
//! text into the markdown even though nothing rendered it in a browser.
//!
//! The same declaration is also the classic inline-block/email spacing hack, where the wrapper
//! kills inter-child whitespace and each child restores a readable size. Removing that subtree
//! would delete visible text, so a descendant re-declaring a non-zero `font-size` keeps it.
//! That guard is a one-level-of-inheritance heuristic, not a cascade.

use html_to_markdown_rs::{ConversionOptions, convert};

fn content(html: &str) -> String {
    convert(html, None).unwrap().content.unwrap_or_default()
}

/// The reported reproduction.
#[test]
fn should_drop_a_font_size_zero_banner() {
    let html = concat!(
        "<p>visible</p>",
        r#"<div style="font-size:0px; color:rgb(255,255,255)">"#,
        r#"<span style="line-height:0">sophospsmartbannerend</span>"#,
        "</div>",
        "<p>also visible</p>"
    );
    let out = content(html);
    assert!(!out.contains("sophospsmartbannerend"), "hidden banner leaked: {out:?}");
    assert!(
        out.contains("visible") && out.contains("also visible"),
        "actual: {out:?}"
    );
}

/// Every spelling of an exact zero length hides; a non-zero size never does.
#[test]
fn should_treat_only_an_exact_zero_length_as_hidden() {
    for zero in [
        "0",
        "0px",
        "0PX",
        "0.0px",
        ".0em",
        "0%",
        "0 ",
        "0pt",
        "0rem",
        "0px !important",
    ] {
        let html = format!(r#"<p>keep</p><div style="font-size:{zero}">gone</div>"#);
        let out = content(html.as_str());
        assert!(!out.contains("gone"), "font-size:{zero} should hide: {out:?}");
    }
    for visible in ["0.5px", "10px", "1em", "medium", "calc(0px)", "inherit", "00.1px"] {
        let html = format!(r#"<p>keep</p><div style="font-size:{visible}">shown</div>"#);
        let out = content(html.as_str());
        assert!(out.contains("shown"), "font-size:{visible} should render: {out:?}");
    }
}

/// The spacing hack: the wrapper is `font-size:0` but the child restores a readable size, so
/// the child's text really does render and must survive.
#[test]
fn should_keep_a_font_size_zero_wrapper_whose_child_restores_a_size() {
    let html = r#"<div style="font-size:0"><span style="font-size:14px">restored</span></div>"#;
    let out = content(html);
    assert!(out.contains("restored"), "spacing-hack content was deleted: {out:?}");
}

/// The guard is scoped to `font-size` alone. `display:none` stays unconditional, so a child
/// restoring a font size does not rescue it.
#[test]
fn should_still_drop_display_none_even_when_a_child_sets_a_font_size() {
    let html = r#"<p>keep</p><div style="display:none"><span style="font-size:14px">gone</span></div>"#;
    let out = content(html);
    assert!(!out.contains("gone"), "display:none must stay unconditional: {out:?}");
    assert!(out.contains("keep"), "actual: {out:?}");
}

/// The CSS cascade applies to `font-size` exactly as it already does to `display`: the last
/// declaration for a property wins.
#[test]
fn should_honour_the_cascade_for_font_size() {
    let overridden = content(r#"<p>keep</p><div style="font-size:0;font-size:14px">shown</div>"#);
    assert!(
        overridden.contains("shown"),
        "later non-zero size must win: {overridden:?}"
    );

    let reinstated = content(r#"<p>keep</p><div style="font-size:14px;font-size:0">gone</div>"#);
    assert!(!reinstated.contains("gone"), "later zero size must win: {reinstated:?}");
}

/// Detection runs before any option is consulted, so it is not something a caller can switch
/// off — the same contract `display:none` has always had.
#[test]
fn should_drop_font_size_zero_regardless_of_options() {
    let html = r#"<p>keep</p><div style="font-size:0">gone</div>"#;
    for options in [
        ConversionOptions::default(),
        ConversionOptions {
            extract_metadata: false,
            ..ConversionOptions::default()
        },
    ] {
        let out = convert(html, Some(options)).unwrap().content.unwrap_or_default();
        assert!(!out.contains("gone"), "actual: {out:?}");
    }
}
