// ~keep Rust inner attributes below are crate-level attributes, not a shell shebang.
#![allow(missing_docs)]
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)] // ~keep: examples print by design
fn convert(
    html: &str,
    opts: Option<html_to_markdown_rs::ConversionOptions>,
) -> html_to_markdown_rs::error::Result<String> {
    html_to_markdown_rs::convert(html, opts).map(|r| r.content.unwrap_or_default())
}

use html_to_markdown_rs::ConversionOptions;

fn print_result(label: &str, html: &str, expected: &str, result: html_to_markdown_rs::error::Result<String>) {
    match result {
        Ok(markdown) => {
            println!("Test - {label}:");
            println!("HTML: {html}");
            println!("Markdown: {markdown}");
            println!("Expected: {expected}");
            println!();
        }
        Err(error) => eprintln!("Error: {error}"),
    }
}

fn main() {
    let html = "<p>This is <mark>highlighted</mark> text</p>";
    print_result(
        "Mark (default)",
        html,
        "This is ==highlighted== text",
        convert(html, None),
    );

    let html2 = "<p>This is <del>deleted</del> and <s>strikethrough</s> text</p>";
    print_result(
        "Del/Strike",
        html2,
        "This is ~~deleted~~ and ~~strikethrough~~ text",
        convert(html2, None),
    );

    let html3 = "<p>This is <ins>inserted</ins> text</p>";
    print_result("Ins", html3, "This is ==inserted== text", convert(html3, None));

    let html4 = "<p>Press <kbd>Ctrl+C</kbd> and see <samp>output</samp></p>";
    print_result(
        "Kbd/Samp",
        html4,
        "Press `Ctrl+C` and see `output`",
        convert(html4, None),
    );

    let html5 = "<p>The variable <var>x</var> is defined</p>";
    print_result("Var", html5, "The variable *x* is defined", convert(html5, None));

    let opts = ConversionOptions {
        sub_symbol: "~".to_string(),
        sup_symbol: "^".to_string(),
        ..Default::default()
    };

    let html6 = "<p>H<sub>2</sub>O and x<sup>2</sup></p>";
    print_result("Sub/Sup", html6, "H~2~O and x^2^", convert(html6, Some(opts)));

    let html7 = r#"<p>The <abbr title="World Health Organization">WHO</abbr> announced</p>"#;
    print_result(
        "Abbr",
        html7,
        "The WHO (World Health Organization) announced",
        convert(html7, None),
    );

    let html8 = "<p>This is <u>underlined</u> and <small>small</small> text</p>";
    print_result(
        "U/Small",
        html8,
        "This is underlined and small text",
        convert(html8, None),
    );
}
