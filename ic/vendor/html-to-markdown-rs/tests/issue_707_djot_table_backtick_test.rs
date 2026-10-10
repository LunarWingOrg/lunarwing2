#![allow(missing_docs)]

use html_to_markdown_rs::{ConversionOptions, OutputFormat, convert};

fn djot(html: &str) -> String {
    let options = ConversionOptions {
        output_format: OutputFormat::Djot,
        ..ConversionOptions::default()
    };
    convert(html, Some(options))
        .expect("conversion should succeed")
        .content
        .unwrap_or_default()
}

#[test]
fn should_escape_a_backtick_in_a_djot_table_text_cell() {
    let output = djot("<table><tr><td>a`b</td><td>z</td></tr></table>");

    assert_eq!(output, "| a\\`b | z |\n|----|---|\n");
}

#[test]
fn should_escape_backticks_and_pipes_in_a_wrapped_djot_table_text_cell() {
    let output = djot("<table><tr><td><span>a`b|c</span></td><td>z</td></tr></table>");

    assert_eq!(output, "| a\\`b\\|c | z |\n|-------|---|\n");
}

#[test]
fn should_not_escape_the_delimiters_of_a_djot_verbatim_span() {
    let output = djot("<table><tr><td><code>a`b</code></td><td>z</td></tr></table>");

    assert_eq!(output, "| ``a`b`` | z |\n|-------|---|\n");
}

#[test]
fn should_escape_a_backtick_in_a_djot_table_image_alt() {
    let output = djot("<table><tr><td><img src=\"x\" alt=\"a`b\"></td><td>z</td></tr></table>");

    assert!(
        output.lines().next().is_some_and(|row| row == r"| ![a\`b](x) | z |"),
        "{output:?}"
    );
}

#[test]
fn should_escape_a_backtick_in_a_djot_table_image_title() {
    let output = djot("<table><tr><td><img src=\"x\" alt=\"a\" title=\"t`u\"></td><td>z</td></tr></table>");

    assert!(
        output
            .lines()
            .next()
            .is_some_and(|row| row == r#"| ![a](x "t\`u") | z |"#),
        "{output:?}"
    );
}

#[test]
fn should_escape_backticks_in_djot_table_graphic_text_attributes() {
    let output =
        djot("<table><tr><td><graphic src=\"x\" alt=\"a`b\" title=\"t`u\"></graphic></td><td>z</td></tr></table>");

    assert!(
        output
            .lines()
            .next()
            .is_some_and(|row| row == r#"| ![a\`b](x "t\`u") | z |"#),
        "{output:?}"
    );
}

#[test]
fn should_escape_a_backtick_in_a_djot_table_link_title() {
    let output = djot("<table><tr><td><a href=\"x\" title=\"t`u\">a</a></td><td>z</td></tr></table>");

    assert!(
        output
            .lines()
            .next()
            .is_some_and(|row| row == r#"| [a](x "t\`u") | z |"#),
        "{output:?}"
    );
}

#[test]
fn should_escape_a_backtick_in_a_djot_table_link_destination() {
    let output = djot("<table><tr><td><a href=\"x`y\">a</a></td><td>z</td></tr></table>");

    assert_eq!(output, "| [a](x\\`y) | z |\n|---------|---|\n");
}

#[test]
fn should_leave_a_djot_link_destination_outside_a_table_unchanged() {
    assert_eq!(djot("<a href=\"x`y\">a</a>"), "[a](x`y)\n");
}

#[test]
fn should_leave_a_markdown_table_link_destination_unchanged() {
    let output = convert(
        "<table><tr><td><a href=\"x`y\">a</a></td><td>z</td></tr></table>",
        Some(ConversionOptions::default()),
    )
    .expect("conversion should succeed")
    .content
    .unwrap_or_default();

    assert_eq!(output, "| [a](x`y) | z |\n| -------- | --- |\n");
}
