// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]
#![cfg(feature = "testkit")]

//! Regression tests for issue #692 follow-up: a `<details>`, `<summary>`, `<menu>` or `<legend>`
//! joined the text around it in a table cell, a list item or a paragraph. Each now separates that
//! text the way a `<div>` does, and a summary or legend stays bold.

use html_to_markdown_rs::options::OutputFormat;
use html_to_markdown_rs::prescan::PrescanReport;
use html_to_markdown_rs::{ConversionOptions, TierStrategy, convert, tier1};

/// Each element and the way it writes `b`.
const ELEMENTS: [(&str, &str); 4] = [
    ("details", "b"),
    ("summary", "**b**"),
    ("menu", "b"),
    ("legend", "**b**"),
];

fn tier2_options(br_in_tables: bool) -> ConversionOptions {
    ConversionOptions {
        extract_metadata: false,
        br_in_tables,
        tier_strategy: TierStrategy::Tier2,
        ..ConversionOptions::default()
    }
}

fn tier2(html: &str, br_in_tables: bool) -> String {
    convert(html, Some(tier2_options(br_in_tables)))
        .expect("conversion must succeed")
        .content
        .unwrap_or_default()
}

/// The one-row table with `row` and its separator row.
fn table(row: &str) -> String {
    let dashes: Vec<String> = row
        .trim_matches('|')
        .split('|')
        .map(|cell| "-".repeat(cell.trim().len().max(3)))
        .collect();
    format!("{row}\n| {} |\n", dashes.join(" | "))
}

fn assert_no_failures(failures: &[String], what: &str) {
    assert!(failures.is_empty(), "{what}:\n{}", failures.join("\n"));
}

#[test]
fn should_separate_the_text_around_the_element_in_a_cell_like_a_div() {
    let mut failures = Vec::new();
    for (element, b) in ELEMENTS {
        let html = format!("<table><tr><td>a<{element}>b</{element}>c</td></tr></table>");
        for (br_in_tables, row) in [(false, format!("| a {b} c |")), (true, format!("| a<br>{b}<br>c |"))] {
            let want = table(&row);
            let tier2_out = tier2(&html, br_in_tables);
            let options = ConversionOptions {
                tier_strategy: TierStrategy::Tier1,
                ..tier2_options(br_in_tables)
            };
            // ~keep Tier 1 leaves a legend in a cell to Tier 2.
            let tier1_out = tier1::run(&html, &PrescanReport::default(), &options);
            let tier1_ok = match &tier1_out {
                Ok(out) => *out == tier2_out,
                Err(tier1::BailReason::TableBlockChildInCell) => element == "legend",
                Err(_) => false,
            };
            if tier2_out != want || !tier1_ok {
                failures.push(format!(
                    "{html:?} br_in_tables={br_in_tables}: tier2 {tier2_out:?} tier1 {tier1_out:?}, want {want:?}"
                ));
            }
        }
    }
    assert_no_failures(&failures, "cell differs from a div");
}

#[test]
fn should_keep_the_element_and_the_text_after_it_in_the_list_item_like_a_div() {
    let mut failures = Vec::new();
    for (element, b) in ELEMENTS {
        let html = format!("<ul><li>a<{element}>b</{element}>c</li></ul>");
        let want = format!("- a\n\n  {b}\n\n  c\n");
        let actual = tier2(&html, false);
        if actual != want {
            failures.push(format!("{html:?}: {actual:?}, want {want:?}"));
        }
    }
    assert_no_failures(&failures, "list item differs from a div");
}

/// Tier 1 either writes what Tier 2 writes or leaves the document to Tier 2.
#[test]
fn should_convert_a_legend_or_summary_outside_a_cell_the_same_in_both_tiers() {
    let mut failures = Vec::new();
    for (html, want) in [
        ("<legend>x</legend>", "**x**\n"),
        (
            "<blockquote>a<legend>b</legend>c</blockquote>",
            "> a\n>\n> **b**\n>\n> c\n",
        ),
        ("<fieldset><legend>t</legend>body</fieldset>", "**t**\n\nbody\n"),
        ("<p><a href=\"u\">l<summary>b</summary>m</a></p>", "[l b m](u)\n"),
        ("<p><a href=\"u\">l<legend>b</legend>m</a></p>", "[l b m](u)\n"),
    ] {
        let tier2_out = tier2(html, false);
        let options = ConversionOptions {
            tier_strategy: TierStrategy::Tier1,
            ..tier2_options(false)
        };
        let tier1_out = tier1::run(html, &PrescanReport::default(), &options);
        let tier1_agrees = tier1_out.as_ref().map_or(true, |out| *out == tier2_out);
        if tier2_out != want || !tier1_agrees {
            failures.push(format!(
                "{html:?}: tier2 {tier2_out:?} tier1 {tier1_out:?}, want {want:?}"
            ));
        }
    }
    assert_no_failures(&failures, "tiers differ");
}

/// A legend is left out: HTML parsing keeps it inside the paragraph, so the bold is not split.
#[test]
fn should_close_and_reopen_bold_around_the_element_like_a_div() {
    let mut failures = Vec::new();
    for element in ["details", "summary", "menu", "div"] {
        let html = format!("<p><b>a<{element}>b</{element}>c</b></p>");
        let actual = tier2(&html, false);
        if actual != "**a**\n\n**b**\n\n**c**\n" {
            failures.push(format!("{html:?}: {actual:?}"));
        }
    }
    assert_no_failures(&failures, "bold differs from a div");
}

#[test]
fn should_keep_the_element_on_its_own_line_in_plain_output_like_a_div() {
    let mut failures = Vec::new();
    for (element, _) in ELEMENTS {
        let html = format!("<p><b>a<{element}>b</{element}>c</b></p>");
        let options = ConversionOptions {
            output_format: OutputFormat::Plain,
            ..tier2_options(false)
        };
        let actual = convert(&html, Some(options)).expect("conversion must succeed").content;
        if actual.as_deref() != Some("a\n\nb\n\nc\n") {
            failures.push(format!("{html:?}: {actual:?}"));
        }
    }
    assert_no_failures(&failures, "plain output differs from a div");
}

#[test]
fn should_start_the_element_on_a_new_paragraph_like_a_div() {
    let mut failures = Vec::new();
    for (element, b) in ELEMENTS {
        let html = format!("<p>a<{element}>b</{element}>c</p>");
        let want = format!("a\n\n{b}\n\nc\n");
        let actual = tier2(&html, false);
        if actual != want {
            failures.push(format!("{html:?}: {actual:?}, want {want:?}"));
        }
    }
    assert_no_failures(&failures, "paragraph differs from a div");
}
