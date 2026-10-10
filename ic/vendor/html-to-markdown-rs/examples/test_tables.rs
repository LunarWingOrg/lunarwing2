// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)] // ~keep: examples print by design

//! Example: Converting HTML tables to Markdown

fn convert(
    html: &str,
    opts: Option<html_to_markdown_rs::ConversionOptions>,
) -> html_to_markdown_rs::error::Result<String> {
    html_to_markdown_rs::convert(html, opts).map(|r| r.content.unwrap_or_default())
}

fn print_table_result(label: &str, html: &str, expected_lines: &[&str]) {
    match convert(html, None) {
        Ok(markdown) => {
            println!("Test - {label}:");
            println!("HTML: {html}");
            println!("\nMarkdown:\n{markdown}");
            println!("Expected:");
            for line in expected_lines {
                println!("{line}");
            }
            println!();
        }
        Err(error) => eprintln!("Error: {error}"),
    }
}

fn main() {
    let html = r"<table>
        <tr>
            <th>Name</th>
            <th>Age</th>
        </tr>
        <tr>
            <td>Alice</td>
            <td>30</td>
        </tr>
        <tr>
            <td>Bob</td>
            <td>25</td>
        </tr>
    </table>";

    print_table_result(
        "Simple table with header",
        html,
        &["| Name | Age |", "| --- | --- |", "| Alice | 30 |", "| Bob | 25 |"],
    );

    let html2 = r#"<table>
        <tr>
            <th colspan="2">Full Name</th>
            <th>Age</th>
        </tr>
        <tr>
            <td>Alice</td>
            <td>Smith</td>
            <td>30</td>
        </tr>
    </table>"#;

    print_table_result(
        "Table with colspan",
        html2,
        &["| Full Name | | Age |", "| --- | --- | --- |", "| Alice | Smith | 30 |"],
    );

    let html3 = r"<table>
        <thead>
            <tr>
                <th>Product</th>
                <th>Price</th>
            </tr>
        </thead>
        <tbody>
            <tr>
                <td>Widget</td>
                <td>$10</td>
            </tr>
            <tr>
                <td>Gadget</td>
                <td>$15</td>
            </tr>
        </tbody>
    </table>";

    print_table_result(
        "Table with thead/tbody",
        html3,
        &[
            "| Product | Price |",
            "| --- | --- |",
            "| Widget | $10 |",
            "| Gadget | $15 |",
        ],
    );
}
