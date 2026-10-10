//! Synchronous text wrapping for Markdown output.

use super::utils::{
    hard_break, is_heading, is_list_like, is_non_interrupting_ordered_item, is_numbered_list, joins_into_a_block,
    parse_blockquote_line, parse_list_item, push_paragraph_line, wrap_blockquote_paragraph, wrap_indented_line,
    wrap_list_item,
};
use crate::converter::utility::escaping::{code_fence, is_heading_underline, opens_block};
use crate::options::ConversionOptions;

/// Whether `trimmed` closes a fenced code block opened by a run of `length` `marker` characters.
fn closes_fence(trimmed: &str, marker: u8, length: usize) -> bool {
    code_fence(trimmed).is_some_and(|(fence, run)| fence == marker && run >= length && trimmed[run..].trim().is_empty())
}

/// The paragraph the reflow is collecting: plain text, or the text of a list item.
#[derive(Default)]
struct OpenParagraph {
    indent: String,
    /// The list item's marker, or empty for a plain paragraph.
    marker: String,
    text: String,
}

impl OpenParagraph {
    const fn is_open(&self) -> bool {
        !self.text.is_empty()
    }

    const fn is_item(&self) -> bool {
        !self.marker.is_empty()
    }

    fn open(&mut self, indent: &str, marker: &str, line: &str) {
        self.indent.clear();
        self.indent.push_str(indent);
        self.marker.clear();
        self.marker.push_str(marker);
        self.push_line(line);
    }

    fn push_line(&mut self, line: &str) {
        push_paragraph_line(&mut self.text, line);
    }

    /// Whether `line`, which follows the paragraph without a blank line, continues it; `trimmed`
    /// is `line` without its indentation.
    ///
    /// ~keep A line that cannot start a block is paragraph continuation text in CommonMark, at
    /// ~keep column 0 too (a lazy line), so it stays in the paragraph or the list item (#616).
    /// ~keep A list item line left of the text's column leaves the item and starts a list at any
    /// ~keep number; at or right of that column only a line that interrupts a paragraph does.
    fn continues_with(&self, line: &str, trimmed: &str) -> bool {
        !trimmed.is_empty()
            && (!opens_block(trimmed) || self.is_lazy_equals_underline(line, trimmed))
            && !trimmed.starts_with('|')
            && parse_list_item(line).is_none_or(|(indent, _, _)| indent.len() >= self.indent.len() + self.marker.len())
    }

    /// Whether `line` is a run of `=` left of the list item's text column.
    ///
    /// ~keep A setext underline cannot be a lazy line (CommonMark 4.3), so there it is paragraph
    /// ~keep text; cutting the paragraph at it made the lines after it lose their hard breaks
    /// ~keep (issue #680). A `-` run left of the column is a thematic break and ends the item.
    /// Only when the run joins the line above: on a line of its own at the item's column, after a
    /// hard break or after a bare marker the run would make a list item (`1.`), it would underline
    /// the text above, so the paragraph ends there and keeps its hard break (`flush_before_line`).
    fn is_lazy_equals_underline(&self, line: &str, trimmed: &str) -> bool {
        self.is_item()
            && !self.text.ends_with('\n')
            && line.len() - trimmed.len() < self.indent.len() + self.marker.len()
            && trimmed.trim_end().bytes().all(|byte| byte == b'=')
            && !joins_into_a_block(&self.text, trimmed.trim_matches([' ', '\t']))
    }

    /// Write the paragraph: a plain one ends with a blank line, a list item with its line end.
    fn flush(&mut self, out: &mut String, width: usize) {
        if self.text.is_empty() {
            return;
        }
        if self.is_item() {
            out.push_str(&wrap_list_item(&self.indent, &self.marker, &self.text, width));
        } else {
            out.push_str(&wrap_indented_line(&self.indent, &self.text, width));
            out.push_str("\n\n");
        }
        self.text.clear();
    }

    /// Write the paragraph before a line that ends it without a blank line between them.
    ///
    /// ~keep A list item's text that ends with a hard break keeps it: the line after it can still
    /// ~keep be text of the item, a lazy `===` line after a break (`- a  \n===`), so dropped, the
    /// ~keep break would be lost (issue #680). Before a line that ends the item it has no effect.
    fn flush_before_line(&mut self, out: &mut String, width: usize) {
        let spaces = if self.is_item() {
            self.text
                .strip_suffix('\n')
                .map(|text| text.len() - text.trim_end_matches(' ').len())
        } else {
            None
        };
        self.flush(out, width);
        if let Some(spaces) = spaces.filter(|&spaces| spaces >= 2) {
            out.pop();
            out.extend(std::iter::repeat_n(' ', spaces));
            out.push('\n');
        }
    }
}

#[derive(Default)]
struct BlockquoteState {
    open_fence: Option<(u8, usize)>,
    in_paragraph: bool,
    prefix: String,
    indent: String,
    buffer: String,
}

impl BlockquoteState {
    fn flush(&mut self, out: &mut String, width: usize) {
        if !self.in_paragraph || self.buffer.is_empty() {
            return;
        }
        out.push_str(&wrap_blockquote_paragraph(
            &format!("{}{}", self.prefix, self.indent),
            &self.buffer,
            width,
        ));
        out.push('\n');
        self.buffer.clear();
        self.in_paragraph = false;
    }

    fn flush_before_block(&mut self, out: &mut String, width: usize, underline: bool) {
        if !self.in_paragraph || self.buffer.is_empty() {
            return;
        }
        out.push_str(&wrap_blockquote_paragraph(
            &format!("{}{}", self.prefix, self.indent),
            &self.buffer,
            if underline { usize::MAX } else { width },
        ));
        if let Some(text) = self.buffer.strip_suffix('\n') {
            out.push_str(&text[text.trim_end_matches(' ').len()..]);
        }
        out.push('\n');
        self.buffer.clear();
        self.in_paragraph = false;
    }

    fn process(&mut self, line: &str, prefix: String, content: String, out: &mut String, width: usize) {
        // ~keep The indent after the quote prefix is a list item's content column inside
        // ~keep the quote; written without it, the paragraph would leave the item.
        let after_prefix = &line[prefix.len()..];
        let indent = &after_prefix[..after_prefix.len() - after_prefix.trim_start().len()];
        let mut normalized_prefix = prefix;
        if !normalized_prefix.ends_with(' ') {
            normalized_prefix.push(' ');
        }

        if content.is_empty() {
            self.flush(out, width);
            out.push_str(normalized_prefix.trim_end());
            out.push('\n');
            return;
        }
        if self.in_paragraph && normalized_prefix != self.prefix {
            self.flush(out, width);
        }
        if let Some((marker, length)) = self.open_fence {
            if closes_fence(&content, marker, length) {
                self.open_fence = None;
            }
            push_verbatim_line(out, line);
            return;
        }

        // ~keep Inside a quote a line that starts a block keeps its own line, as outside one.
        // ~keep A hard break before such a line stays because the reflow cannot infer item columns.
        let underline = self.in_paragraph && is_heading_underline(&content);
        let fence = code_fence(&content);
        let list_item = parse_list_item(&content).is_some();
        let continues_numbered_paragraph =
            self.in_paragraph && self.buffer.ends_with('\n') && is_non_interrupting_ordered_item(&content);
        if underline
            || fence.is_some()
            || (opens_block(&content) && !continues_numbered_paragraph)
            || (list_item && !continues_numbered_paragraph)
            || content.starts_with('|')
        {
            self.flush_before_block(out, width, underline);
            self.open_fence = fence;
            push_verbatim_line(out, line);
            return;
        }
        if !self.in_paragraph {
            self.prefix = normalized_prefix;
            self.indent.clear();
            self.indent.push_str(indent);
            self.in_paragraph = true;
        }
        push_paragraph_line(&mut self.buffer, after_prefix);
    }
}

struct MarkdownWrapper {
    width: usize,
    result: String,
    open_fence: Option<(u8, usize)>,
    paragraph: OpenParagraph,
    blockquote: BlockquoteState,
}

impl MarkdownWrapper {
    fn new(capacity: usize, width: usize) -> Self {
        Self {
            width,
            result: String::with_capacity(capacity),
            open_fence: None,
            paragraph: OpenParagraph::default(),
            blockquote: BlockquoteState::default(),
        }
    }

    fn process_line(&mut self, line: &str) {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if self.process_open_fence(line, trimmed) || self.continue_paragraph(line, trimmed) {
            return;
        }
        if self.process_code_line(line, trimmed) {
            return;
        }
        if let Some((prefix, content)) = parse_blockquote_line(line) {
            self.paragraph.flush(&mut self.result, self.width);
            self.blockquote
                .process(line, prefix, content, &mut self.result, self.width);
            return;
        }

        self.blockquote.open_fence = None;
        self.blockquote.flush(&mut self.result, self.width);
        if self.process_setext_heading(line, trimmed)
            || self.process_list_item(line)
            || self.process_structural_line(line, trimmed)
            || self.process_blank_line(line)
        {
            return;
        }
        self.paragraph.open(&line[..line.len() - trimmed.len()], "", line);
    }

    fn process_open_fence(&mut self, line: &str, trimmed: &str) -> bool {
        let Some((marker, length)) = self.open_fence else {
            return false;
        };
        if closes_fence(trimmed, marker, length) {
            self.open_fence = None;
        }
        push_verbatim_line(&mut self.result, line);
        true
    }

    fn continue_paragraph(&mut self, line: &str, trimmed: &str) -> bool {
        if !self.paragraph.is_open() || !self.paragraph.continues_with(line, trimmed) {
            return false;
        }
        self.paragraph.push_line(line);
        true
    }

    fn process_code_line(&mut self, line: &str, trimmed: &str) -> bool {
        let fence = code_fence(trimmed);
        let is_indented_code = line.starts_with("    ")
            && !is_list_like(trimmed)
            && !is_numbered_list(trimmed)
            && !is_heading(trimmed)
            && !trimmed.starts_with('>')
            && !trimmed.starts_with('|');
        if fence.is_none() && !is_indented_code {
            return false;
        }
        self.paragraph.flush(&mut self.result, self.width);
        self.open_fence = fence;
        push_verbatim_line(&mut self.result, line);
        true
    }

    fn process_setext_heading(&mut self, line: &str, trimmed: &str) -> bool {
        if !self.paragraph.is_open() || self.paragraph.is_item() || !is_heading_underline(trimmed) {
            return false;
        }
        // ~keep An underline directly below paragraph text turns it into a heading, so the text
        // ~keep remains unwrapped and ends with one line break rather than a blank line (#607).
        self.paragraph.flush(&mut self.result, usize::MAX);
        self.result.pop();
        push_verbatim_line(&mut self.result, line);
        true
    }

    fn process_list_item(&mut self, line: &str) -> bool {
        let Some((indent, mut marker, mut content)) = parse_list_item(line) else {
            return false;
        };
        // ~keep Nested markers on one physical line form one prefix and set the continuation column.
        while let Some((_, inner_marker, inner_content)) = parse_list_item(&content) {
            marker.push_str(&inner_marker);
            content = inner_content;
        }
        self.paragraph.flush(&mut self.result, self.width);
        if content.is_empty() {
            let mut item = wrap_list_item(&indent, &marker, &content, self.width);
            // ~keep Parsing trims an empty item's trailing whitespace, but an explicit
            // ~keep two-space break still separates it from the escaped line after it (#679).
            if let Some(spaces) = hard_break(line) {
                let _ = item.pop();
                item.push_str(spaces);
                item.push('\n');
            }
            self.result.push_str(&item);
        } else if opens_block(&content) {
            // ~keep Reflowing heading, fence, quote, or rule content would change its structure.
            self.open_fence = code_fence(&content);
            push_verbatim_line(&mut self.result, line);
        } else {
            self.paragraph.open(&indent, &marker, &content);
        }
        true
    }

    fn process_structural_line(&mut self, line: &str, trimmed: &str) -> bool {
        let is_structural =
            is_heading(trimmed) || opens_block(trimmed) || trimmed.starts_with('|') || trimmed.starts_with('=');
        if !is_structural {
            return false;
        }
        self.paragraph.flush_before_line(&mut self.result, self.width);
        push_verbatim_line(&mut self.result, line);
        true
    }

    fn process_blank_line(&mut self, line: &str) -> bool {
        if !line.trim().is_empty() {
            return false;
        }
        let was_plain = self.paragraph.is_open() && !self.paragraph.is_item();
        self.paragraph.flush(&mut self.result, self.width);
        if !was_plain {
            self.result.push('\n');
        }
        true
    }

    fn finish(mut self) -> String {
        self.blockquote.flush(&mut self.result, self.width);
        self.paragraph.flush(&mut self.result, self.width);
        self.result
    }
}

fn push_verbatim_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// Wrap text at specified width while preserving Markdown formatting.
///
/// This function wraps paragraphs of text at the specified width, but:
/// - Does not break long words
/// - Does not break on hyphens
/// - Preserves Markdown formatting (links, bold, etc.)
/// - Only wraps paragraph content, not headers, lists, code blocks, etc.
#[must_use]
pub fn wrap_markdown(markdown: &str, options: &ConversionOptions) -> String {
    if !options.wrap {
        return markdown.to_string();
    }

    let mut wrapper = MarkdownWrapper::new(markdown.len(), options.wrap_width);
    for line in markdown.lines() {
        wrapper.process_line(line);
    }
    wrapper.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_markdown_disabled() {
        let markdown = "This is a very long line that would normally be wrapped at 40 characters";
        let options = ConversionOptions {
            wrap: false,
            ..Default::default()
        };
        let result = wrap_markdown(markdown, &options);
        assert_eq!(result, markdown);
    }

    fn wrap_at_20(markdown: &str) -> String {
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 20,
            ..Default::default()
        };
        wrap_markdown(markdown, &options)
    }

    #[test]
    fn wrap_markdown_keeps_a_thematic_break_on_its_own_line() {
        assert_eq!(wrap_at_20("t\n\n---\nB\n"), "t\n\n---\nB\n\n");
        assert_eq!(wrap_at_20("***\nB\n"), "***\nB\n\n");
        assert_eq!(wrap_at_20("___\nB\n"), "___\nB\n\n");
    }

    #[test]
    fn wrap_markdown_keeps_a_setext_underline_under_its_heading_text() {
        assert_eq!(wrap_at_20("Heading\n-------\n\nx\n"), "Heading\n-------\n\nx\n\n");
        assert_eq!(wrap_at_20("Heading\n=======\n\nx\n"), "Heading\n=======\n\nx\n\n");
    }

    #[test]
    fn wrap_markdown_keeps_a_rule_and_an_underline_on_their_own_lines_in_a_quote() {
        assert_eq!(wrap_at_20("> t\n>\n> ---\n>  B\n"), "> t\n>\n> ---\n>  B\n");
        assert_eq!(wrap_at_20("> Heading\n> -------\n> x\n"), "> Heading\n> -------\n> x\n");
        let long_heading = "> one two three four five six\n> -------\n";
        assert_eq!(wrap_at_20(long_heading), long_heading);
    }

    #[test]
    fn wrap_markdown_keeps_a_fence_open_until_a_fence_as_long_as_its_opener() {
        let fenced = "````\n```\none two three four five six\n````\n";
        assert_eq!(wrap_at_20(fenced), fenced);
        let info_string = "```\n```text\none two three four five six\n```\n";
        assert_eq!(wrap_at_20(info_string), info_string);
        let quoted = "> ~~~\n> ```\n> one two three four five six\n> ~~~\n";
        assert_eq!(wrap_at_20(quoted), quoted);
        assert_eq!(
            wrap_at_20("> ```\n> x\n> ```\n> one two three four five six\n"),
            "> ```\n> x\n> ```\n> one two three four\n> five six\n"
        );
        assert_eq!(
            wrap_at_20("```\nx\n```\none two three four five six\n"),
            "```\nx\n```\none two three four\nfive six\n\n"
        );
    }

    #[test]
    fn wrap_markdown_ends_a_paragraph_or_an_item_only_at_a_line_that_starts_a_block() {
        assert_eq!(
            wrap_at_20("text\n| a | b |\n| --- | --- |\n"),
            "text\n\n| a | b |\n| --- | --- |\n"
        );
        assert_eq!(wrap_at_20("1. a\n\n   para\n2. b\n"), "1. a\n\n   para\n\n2. b\n");
        assert_eq!(wrap_at_20("- a\n---\n"), "- a\n---\n");
        assert_eq!(wrap_at_20("- \n- b\n"), "-\n- b\n");
    }

    #[test]
    fn wrap_markdown_ends_an_item_at_a_number_line_only_left_of_its_text() {
        assert_eq!(wrap_at_20("- a  \n  1990. b\n"), "- a  \n  1990. b\n");
        assert_eq!(wrap_at_20("1. a\\\n   57) b\n"), "1. a\\\n   57) b\n");
        assert_eq!(
            wrap_at_20("1. first\n\n   Released in  \n   2004. Updated later.\n"),
            "1. first\n\n   Released in  \n   2004. Updated\n   later.\n\n"
        );
        assert_eq!(wrap_at_20("- a\n1990. b\n"), "- a\n1990. b\n");
        assert_eq!(wrap_at_20("1. a\n  2. b\n"), "1. a\n  2. b\n");
        assert_eq!(wrap_at_20("1. a\n\n   p\n  2. b\n"), "1. a\n\n   p\n\n  2. b\n");
    }

    #[test]
    fn wrap_markdown_keeps_a_break_after_an_empty_ordered_item() {
        let markdown = "- a  \n  ===\n  2) ---  \n  1990.  \n  \\>\n";
        assert_eq!(wrap_at_20(markdown), "- a  \n  ===\n  2) ---  \n  1990.  \n  \\>\n\n");
    }

    #[test]
    fn wrap_markdown_keeps_a_lazy_equals_line_in_the_list_item_paragraph() {
        // ~keep Issue #680: cut at the `===` line, the item lost the hard break after `1990.`.
        assert_eq!(
            wrap_at_20("- a\n===  \n  1990.  \n  b\n"),
            "- a ===  \n  1990.  \n  b\n"
        );
        // ~keep At the item's text column, or under an indented plain paragraph, the run is an
        // ~keep underline and keeps its own line.
        assert_eq!(wrap_at_20("- a\n  ===\n"), "- a\n  ===\n");
        assert_eq!(wrap_at_20("  a\n===\n"), "  a\n===\n");
        // ~keep After a hard break the run would start a line of its own at the item's column
        // ~keep and underline the text above, so the paragraph ends before it, the run stays
        // ~keep left of the column and the break stays.
        assert_eq!(wrap_at_20("- a  \n===\n"), "- a  \n===\n");
        assert_eq!(wrap_at_20("- a\n===  \n===\n"), "- a ===  \n===\n");
        assert_eq!(wrap_at_20("- q  \n  1.\n===\n"), "- q  \n  1.\n===\n");
    }

    #[test]
    fn wrap_markdown_keeps_a_hard_break_before_a_number_line_in_a_quote() {
        assert_eq!(wrap_at_20("> a  \n> 1990. b\n"), "> a  \n> 1990. b\n");
        assert_eq!(wrap_at_20("> 2. b\n"), "> 2. b\n");
        assert_eq!(wrap_at_20("> - a\n>   2. b\n"), "> - a\n>   2. b\n");
        assert_eq!(wrap_at_20("> - x\n> lazy\n> 2. b\n"), "> - x\n> lazy\n> 2. b\n");
        assert_eq!(wrap_at_20("> a\\\n> 1990. b\n"), "> a\\\n> 1990. b\n");
    }

    #[test]
    fn wrap_markdown_keeps_an_item_of_non_breaking_spaces() {
        assert_eq!(wrap_at_20("- \u{a0}\n"), "- \u{a0}\n");
    }

    #[test]
    fn wrap_markdown_keeps_a_nested_marker_on_the_line_of_its_text() {
        assert_eq!(
            wrap_at_20("- 1. [vote](https://example.com/vote) title\n"),
            "- 1. [vote](https://example.com/vote)\n     title\n"
        );
        assert_eq!(wrap_at_20("- 1.\n"), "- 1.\n");
        assert_eq!(
            wrap_at_20("- ## one two three four five\n"),
            "- ## one two three four five\n"
        );
        let fenced = "- ```\n  one two three four five six\n  ```\nafter\n";
        assert_eq!(
            wrap_at_20(fenced),
            "- ```\n  one two three four five six\n  ```\nafter\n\n"
        );
    }

    #[test]
    fn wrap_markdown_reads_a_non_breaking_space_as_text() {
        assert_eq!(wrap_at_20("a\n\u{a0}- b c\n"), "a \u{a0}- b c\n\n");
        assert_eq!(wrap_at_20("\u{a0}- b c\n"), "\u{a0}- b c\n\n");
        assert_eq!(
            wrap_at_20("\u{a0}one two three four five six\n"),
            "\u{a0}one two three four\nfive six\n\n"
        );
    }

    #[test]
    fn wrap_markdown_still_joins_ordinary_paragraph_lines() {
        assert_eq!(wrap_at_20("a\nb--\n"), "a b--\n\n");
    }

    #[test]
    fn test_wrap_markdown_paragraph() {
        let markdown = "This is a very long line that would normally be wrapped at 40 characters\n\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 40,
            ..Default::default()
        };
        let result = wrap_markdown(markdown, &options);
        assert!(result.lines().all(|line| line.len() <= 40 || line.trim().is_empty()));
    }

    #[test]
    fn test_wrap_markdown_blockquote_paragraph() {
        let markdown = "> This is a very long blockquote line that should wrap at 30 characters\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 30,
            ..Default::default()
        };
        let result = wrap_markdown(markdown, &options);
        assert!(
            result.lines().all(|line| line.len() <= 30 || line.trim().is_empty()),
            "Some lines exceed wrap width. Got: {result}"
        );
        assert!(
            result.contains("> This is a very"),
            "Missing expected wrapped content. Got: {result}"
        );
        assert!(
            result.lines().filter(|l| l.starts_with("> ")).count() >= 2,
            "Expected multiple wrapped blockquote lines. Got: {result}"
        );
    }

    #[test]
    fn test_wrap_markdown_preserves_code() {
        let markdown = "```\nThis is a very long line in a code block that should not be wrapped\n```\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 40,
            ..Default::default()
        };
        let result = wrap_markdown(markdown, &options);
        assert!(result.contains("This is a very long line in a code block that should not be wrapped"));
    }

    #[test]
    fn test_wrap_markdown_preserves_headings() {
        let markdown = "# This is a very long heading that should not be wrapped even if it exceeds the width\n\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 40,
            ..Default::default()
        };
        let result = wrap_markdown(markdown, &options);
        assert!(
            result.contains("# This is a very long heading that should not be wrapped even if it exceeds the width")
        );
    }

    #[test]
    fn wrap_markdown_wraps_long_list_items() {
        let markdown = "- This is a very long list item that should definitely be wrapped when it exceeds the specified wrap width\n- Short item\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 60,
            ..Default::default()
        };

        let result = wrap_markdown(markdown, &options);

        assert!(
            result.contains("- This is a very long list item that should definitely be\n  wrapped"),
            "First list item not properly wrapped. Got: {result}"
        );
        assert!(
            result.contains("- Short item"),
            "Short list item incorrectly modified. Got: {result}"
        );
    }

    #[test]
    fn wrap_markdown_wraps_ordered_lists() {
        let markdown = "1. This is a numbered list item with a very long text that should be wrapped at the specified width\n2. Short\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 60,
            ..Default::default()
        };

        let result = wrap_markdown(markdown, &options);

        assert!(
            result.lines().all(|line| line.len() <= 60 || line.trim().is_empty()),
            "Some lines exceed wrap width. Got: {result}"
        );
        assert!(result.contains("1."), "Lost ordered list marker. Got: {result}");
        assert!(result.contains("2."), "Lost second ordered list marker. Got: {result}");
    }

    #[test]
    fn wrap_markdown_preserves_nested_list_structure() {
        let markdown = "- Item one with some additional text that will need to be wrapped across multiple lines\n  - Nested item with long text that also needs wrapping at the specified width\n  - Short nested\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 50,
            ..Default::default()
        };

        let result = wrap_markdown(markdown, &options);

        assert!(result.contains("- Item"), "Lost top-level list marker. Got: {result}");
        assert!(
            result.contains("  - Nested"),
            "Lost nested list structure. Got: {result}"
        );
        assert!(
            result.lines().all(|line| line.len() <= 50 || line.trim().is_empty()),
            "Some lines exceed wrap width. Got: {result}"
        );
    }

    #[test]
    fn wrap_markdown_handles_list_with_links() {
        let markdown = "- [A](#a) with additional text that is long enough to require wrapping at the configured width\n  - [B](#b) also has more content that needs wrapping\n  - [C](#c)\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 50,
            ..Default::default()
        };

        let result = wrap_markdown(markdown, &options);

        assert!(result.contains("[A](#a)"), "Lost link in list. Got: {result}");
        assert!(result.contains("[B](#b)"), "Lost nested link. Got: {result}");
        assert!(result.contains("[C](#c)"), "Lost short nested link. Got: {result}");
        assert!(
            result.contains("- [A](#a)"),
            "Lost list structure with link. Got: {result}"
        );
        assert!(
            result.contains("  - [B](#b)"),
            "Lost nested list structure. Got: {result}"
        );
    }

    #[test]
    fn wrap_markdown_handles_empty_list_items() {
        let markdown = "- \n- Item with text\n- \n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 40,
            ..Default::default()
        };

        let result = wrap_markdown(markdown, &options);

        assert!(result.contains("- "), "Lost list markers. Got: {result}");
        assert!(result.contains("Item with text"), "Lost item text. Got: {result}");
    }

    #[test]
    fn wrap_markdown_preserves_indented_lists_with_wrapping() {
        let markdown = "- [A](#a) with some additional text that makes this line very long and should be wrapped\n  - [B](#b)\n  - [C](#c) with more text that is also quite long and needs wrapping\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 50,
            ..Default::default()
        };

        let result = wrap_markdown(markdown, &options);

        assert!(result.contains("- [A](#a)"), "Lost top-level link. Got: {result}");
        assert!(result.contains("  - [B](#b)"), "Lost nested link B. Got: {result}");
        assert!(result.contains("  - [C](#c)"), "Lost nested link C. Got: {result}");
        assert!(
            result.lines().all(|line| line.len() <= 50),
            "Some lines exceed wrap width:\n{result}"
        );
    }

    #[test]
    fn wrap_markdown_does_not_wrap_link_only_items() {
        let markdown = "- [A very long link label that would exceed wrap width](#a-very-long-link-label)\n  - [Nested very long link label that would also exceed](#nested)\n";
        let options = ConversionOptions {
            wrap: true,
            wrap_width: 30,
            ..Default::default()
        };

        let result = wrap_markdown(markdown, &options);

        assert!(result.contains("- [A very long link label that would exceed wrap width](#a-very-long-link-label)"));
        assert!(result.contains("  - [Nested very long link label that would also exceed](#nested)"));
    }
}
