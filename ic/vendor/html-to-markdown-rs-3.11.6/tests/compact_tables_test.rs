// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]

use html_to_markdown_rs::ConversionOptions;

fn convert(html: &str, opts: Option<ConversionOptions>) -> String {
    html_to_markdown_rs::convert(html, opts)
        .expect("conversion failed")
        .content
        .unwrap_or_default()
}

#[test]
fn compact_tables_emits_no_padding() {
    let html = r"<table>
        <thead><tr><th>Name</th><th>Score</th><th>Status</th></tr></thead>
        <tbody>
            <tr><td>Alice</td><td>100</td><td>Active</td></tr>
            <tr><td>Bob</td><td>42</td><td>Inactive</td></tr>
        </tbody>
    </table>";

    let opts = ConversionOptions::builder().compact_tables(true).build();
    let result = convert(html, Some(opts));

    assert!(result.contains("| 42 |"), "short cell should not be padded: {result}");
    assert!(result.contains("| 100 |"), "medium cell should not be padded: {result}");

    assert!(result.contains("| --- |"), "separator should be exactly ---: {result}");
    assert!(!result.contains("| ---- |"), "separator must not be padded: {result}");
}

#[test]
fn compact_tables_false_preserves_padding() {
    let html = r"<table>
        <thead><tr><th>Name</th><th>Score</th></tr></thead>
        <tbody>
            <tr><td>Alice</td><td>100</td></tr>
            <tr><td>Bob</td><td>42</td></tr>
        </tbody>
    </table>";

    let result = convert(html, None);

    assert!(
        result.contains("| 42    |"),
        "default mode must pad short cells: {result}"
    );
    assert!(
        !result.contains("| 42 |"),
        "bare unpadded form must not appear: {result}"
    );
}

#[test]
fn compact_tables_via_apply_update_produces_compact_output() {
    use html_to_markdown_rs::options::ConversionOptionsUpdate;

    let html = r"<table>
        <thead><tr><th>Name</th><th>Score</th></tr></thead>
        <tbody>
            <tr><td>Alice</td><td>100</td></tr>
            <tr><td>Bob</td><td>42</td></tr>
        </tbody>
    </table>";

    let mut opts = ConversionOptions::default();
    opts.apply_update(ConversionOptionsUpdate {
        compact_tables: Some(true),
        ..Default::default()
    });

    let result = convert(html, Some(opts));

    assert!(
        result.contains("| --- |"),
        "separator must be --- via apply_update: {result}"
    );
    assert!(
        !result.contains("| ---- |"),
        "separator must not be padded via apply_update: {result}"
    );
    assert!(
        result.contains("| 42 |"),
        "short cell must not be padded via apply_update: {result}"
    );
}

#[test]
fn compact_tables_separator_is_three_dashes() {
    let html = r"<table>
        <thead><tr><th>A very long header cell</th><th>B</th></tr></thead>
        <tbody><tr><td>x</td><td>y</td></tr></tbody>
    </table>";

    let opts = ConversionOptions::builder().compact_tables(true).build();
    let result = convert(html, Some(opts));

    assert!(
        result.contains("| --- | --- |"),
        "separator must be --- per column regardless of content width: {result}"
    );
}

// ~keep Tables with rowspan/colspan must not panic and must produce valid GFM.
#[test]
fn compact_tables_with_rowspan_does_not_panic() {
    let html = r#"<table>
        <thead><tr><th>A</th><th>B</th></tr></thead>
        <tbody>
            <tr><td rowspan="2">merged</td><td>r1</td></tr>
            <tr><td>r2</td></tr>
        </tbody>
    </table>"#;

    let opts = ConversionOptions::builder().compact_tables(true).build();
    let result = convert(html, Some(opts));

    assert!(result.contains('|'), "result must contain pipe table: {result}");
    assert!(result.contains("| --- |"), "separator must be present: {result}");
}

#[test]
fn compact_tables_single_column() {
    let html = r"<table>
        <thead><tr><th>Only</th></tr></thead>
        <tbody><tr><td>val</td></tr></tbody>
    </table>";

    let opts = ConversionOptions::builder().compact_tables(true).build();
    let result = convert(html, Some(opts));

    assert!(result.contains("| Only |"), "header present: {result}");
    assert!(result.contains("| --- |"), "separator present: {result}");
    assert!(result.contains("| val |"), "value present: {result}");
}

#[cfg(any(feature = "serde", feature = "metadata"))]
#[test]
fn compact_tables_serde_round_trip() {
    let opts = ConversionOptions::builder().compact_tables(true).build();
    let json = serde_json::to_string(&opts).expect("serialize");
    let restored: ConversionOptions = serde_json::from_str(&json).expect("deserialize");
    assert!(restored.compact_tables, "compact_tables must survive serde round-trip");
}

#[cfg(any(feature = "serde", feature = "metadata"))]
#[test]
fn compact_tables_defaults_to_false_when_absent_from_json() {
    let partial = r#"{"wrap": true}"#;
    let opts: ConversionOptions = serde_json::from_str(partial).expect("deserialize partial");
    assert!(!opts.compact_tables, "compact_tables must default to false");
}
