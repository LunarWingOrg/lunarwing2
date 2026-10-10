// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]

use html_to_markdown_rs::{CodeBlockStyle, ConversionOptions, WhitespaceMode};

fn convert(html: &str, opts: Option<ConversionOptions>) -> html_to_markdown_rs::error::Result<String> {
    html_to_markdown_rs::convert(html, opts).map(|r| r.content.unwrap_or_default())
}

fn cell_options(br_in_tables: bool) -> ConversionOptions {
    ConversionOptions {
        br_in_tables,
        compact_tables: true,
        ..Default::default()
    }
}

/// ~keep A fenced code block cannot exist inside a GFM pipe cell, so `<pre>` uses a code span.
#[test]
fn should_replace_code_fence_with_span_when_pre_is_inside_table_cell_and_br_in_tables_is_true() {
    let html = "<table><tr><td><pre>a\nb</pre></td></tr></table>";
    let result = convert(html, Some(cell_options(true))).unwrap();
    assert_eq!(result, "| `a b` |\n| --- |\n", "actual: {result:?}");
    assert!(
        !result.contains("```"),
        "no code fence may appear in a cell: {result:?}"
    );
    assert_eq!(
        result.lines().count(),
        2,
        "table row must stay on one physical line: {result:?}"
    );
}

#[test]
fn should_replace_code_fence_with_span_when_pre_is_inside_table_cell_and_br_in_tables_is_false() {
    let html = "<table><tr><td><pre>a\nb</pre></td></tr></table>";
    let result = convert(html, Some(cell_options(false))).unwrap();
    assert_eq!(result, "| `a b` |\n| --- |\n", "actual: {result:?}");
    assert!(
        !result.contains("```"),
        "no code fence may appear in a cell: {result:?}"
    );
}

#[test]
fn should_use_code_span_when_pre_wraps_a_code_element_inside_table_cell() {
    let html = "<table><tr><td><pre><code>a\nb</code></pre></td></tr></table>";
    let result = convert(html, Some(cell_options(true))).unwrap();
    assert_eq!(result, "| `a b` |\n| --- |\n", "actual: {result:?}");
}

#[test]
fn should_replace_indented_code_block_with_span_when_pre_is_inside_table_cell() {
    let html = "<table><tr><td><pre>a\nb</pre>tail</td></tr></table>";
    let options = ConversionOptions {
        code_block_style: CodeBlockStyle::Indented,
        br_in_tables: true,
        compact_tables: true,
        ..Default::default()
    };
    let result = convert(html, Some(options)).unwrap();
    assert_eq!(result, "| `a b`<br>tail |\n| --- |\n", "actual: {result:?}");
    assert_eq!(
        result.lines().count(),
        2,
        "table row must stay on one physical line: {result:?}"
    );
}

#[test]
fn should_replace_tilde_fence_with_span_when_pre_is_inside_table_cell() {
    let html = "<table><tr><td><pre>a\nb</pre></td></tr></table>";
    let options = ConversionOptions {
        code_block_style: CodeBlockStyle::Tildes,
        compact_tables: true,
        ..Default::default()
    };
    let result = convert(html, Some(options)).unwrap();
    assert_eq!(result, "| `a b` |\n| --- |\n", "actual: {result:?}");
    assert!(
        !result.contains("~~~"),
        "no tilde fence may appear in a cell: {result:?}"
    );
}

/// A fence's language info string is part of the dropped block syntax: with no fence to carry
/// it, emitting `rust` would leak the class name into the cell as stray text.
#[test]
fn should_not_emit_language_info_string_when_pre_is_inside_table_cell() {
    let html = "<table><tr><td><pre class=\"language-rust\">a\nb</pre></td></tr></table>";
    let result = convert(html, Some(cell_options(true))).unwrap();
    assert_eq!(result, "| `a b` |\n| --- |\n", "actual: {result:?}");
    assert!(
        !result.contains("rust"),
        "language must not leak into the cell: {result:?}"
    );
}

#[test]
fn should_widen_span_delimiters_around_backticks_in_pre_content_inside_table_cell() {
    let html = "<table><tr><td><pre>a`b</pre></td></tr></table>";
    let result = convert(html, Some(cell_options(true))).unwrap();
    assert_eq!(result, "| ``a`b`` |\n| --- |\n", "actual: {result:?}");
}

/// The fold must hold under `Strict` too: `whitespace_mode` does not make a raw newline legal
/// between two pipes.
#[test]
fn should_use_code_span_when_pre_is_inside_table_cell_under_strict_whitespace_mode() {
    let options = ConversionOptions {
        br_in_tables: true,
        whitespace_mode: WhitespaceMode::Strict,
        compact_tables: true,
        ..Default::default()
    };
    let html = "<table><tr><td><pre>a\nb</pre></td></tr></table>";
    let result = convert(html, Some(options)).unwrap();
    assert_eq!(result, "| `a b` |\n| --- |\n", "actual: {result:?}");
    assert_eq!(
        result.lines().count(),
        2,
        "table row must stay on one physical line: {result:?}"
    );
}

/// A `<pre>` outside any table cell must still render as a normal fenced code block, keeping
/// its newlines and language — the regression guard against the in-cell degradation leaking
/// into the ordinary code-block path.
#[test]
fn should_keep_fenced_code_block_when_pre_is_outside_table_cell() {
    let result = convert("<div><pre>a\nb</pre></div>", Some(cell_options(true))).unwrap();
    assert_eq!(result, "```\na\nb\n```\n", "actual: {result:?}");

    let with_language = convert(
        "<div><pre class=\"language-rust\">a\nb</pre></div>",
        Some(cell_options(true)),
    )
    .unwrap();
    assert_eq!(with_language, "```rust\na\nb\n```\n", "actual: {with_language:?}");
}
