//! Handler for ruby annotation inline elements (ruby, rb, rt, rp, rtc).
//!
//! Converts HTML ruby annotation elements to Markdown format with support for:
//! - Ruby base text elements (<ruby>, <rb>)
//! - Ruby text annotations (<rt>) for phonetic guidance (common in CJK)
//! - Ruby parentheses (<rp>) for fallback presentation in browsers without ruby support
//! - Ruby text container (<rtc>) for secondary annotations or separate ruby text grouping
//! - Interleaved rendering mode: rb/rt pairs rendered inline (rb1(rt1)rb2(rt2))
//! - Grouped rendering mode: all rb text followed by rt annotations in parentheses
//! - Proper handling of CJK (Chinese/Japanese/Korean) text with multiple annotations
//! - Visitor callbacks for custom ruby processing
//! - Whitespace normalization and trimming

use crate::converter::inline::HandlerContext;

type Context = crate::converter::Context;

/// Handles ruby annotation elements: ruby, rb, rt, rp, rtc.
///
/// Ruby annotations are used in East Asian typography to show pronunciation guides
/// or provide alternate text. The handler supports two rendering modes:
///
/// # Rendering Modes
///
/// **Interleaved mode** (when rb and rt elements are alternated without rtc):
/// - Renders ruby text inline with base text: `base(annotation)base(annotation)`
/// - Example: `<ruby><rb>漢</rb><rt>かん</rt></ruby>` → `漢(かん)`
///
/// **Grouped mode** (when rtc is present or rb/rt are not interleaved):
/// - Renders all base text first, then all annotations in parentheses: `base(annotation1annotation2)`
/// - Handles multiple rt elements and rtc (ruby text container) grouping
/// - Example: `<ruby><rb>東</rb><rb>京</rb><rt>とう</rt><rt>きょう</rt></ruby>` → `東京(とうきょう)`
///
/// # Element Handling
///
/// - `<ruby>`: Main container, detects layout and delegates to appropriate rendering mode
/// - `<rb>`: Base text; content is extracted and used in output
/// - `<rt>`: Annotation text; wrapped in parentheses in standalone contexts
/// - `<rp>`: Ruby parentheses (fallback for browsers without ruby support); skipped in most contexts
/// - `<rtc>`: Ruby text container for grouped annotations; content extracted after rt annotations
///
/// # Note
/// This function references `walk_node` and `normalized_tag_name` from converter.rs,
/// which must be accessible (pub(crate)) for this module to work correctly.
pub fn handle(tag_name: &str, mut handler: HandlerContext<'_>) {
    let Some(node) = handler.node_handle.get(handler.parser) else {
        return;
    };

    let tag = match node {
        tl::Node::Tag(tag) => tag,
        _ => return,
    };

    match tag_name {
        "ruby" => handle_ruby(tag, &mut handler),
        "rb" => handle_base(tag, &mut handler),
        "rt" => handle_annotation(tag, &mut handler),
        "rp" => handle_parenthesis(tag, &mut handler),
        "rtc" => walk_children_to_output(tag, &mut handler),
        _ => walk_children_to_output(tag, &mut handler),
    }
}

fn handle_ruby(tag: &tl::HTMLTag<'_>, handler: &mut HandlerContext<'_>) {
    let ruby_context = handler.context.inline_buffer(handler.output, false);
    let tag_sequence = ruby_tag_sequence(tag, handler.parser);
    let has_rtc = tag_sequence.iter().any(|tag| tag == "rtc");
    let is_interleaved = tag_sequence.windows(2).any(|tags| tags[0] == "rb" && tags[1] == "rt");

    if is_interleaved && !has_rtc {
        render_interleaved(tag, &ruby_context, handler);
    } else {
        render_grouped(tag, &ruby_context, has_rtc, handler);
    }
}

fn ruby_tag_sequence(tag: &tl::HTMLTag<'_>, parser: &tl::Parser<'_>) -> Vec<String> {
    tag.children()
        .top()
        .iter()
        .filter_map(|child_handle| {
            let tl::Node::Tag(child_tag) = child_handle.get(parser)? else {
                return None;
            };
            let tag_name = crate::converter::normalized_tag_name(child_tag.name().as_utf8_str());
            matches!(tag_name.as_ref(), "rb" | "rt" | "rtc").then(|| tag_name.into_owned())
        })
        .collect()
}

fn render_interleaved(tag: &tl::HTMLTag<'_>, ruby_context: &Context, handler: &mut HandlerContext<'_>) {
    let mut current_base = String::new();
    for child_handle in tag.children().top().iter() {
        render_interleaved_child(child_handle, &mut current_base, ruby_context, handler);
    }
    flush_base(&mut current_base, handler.output);
}

fn render_interleaved_child(
    child_handle: &tl::NodeHandle,
    current_base: &mut String,
    ruby_context: &Context,
    handler: &mut HandlerContext<'_>,
) {
    match child_handle.get(handler.parser) {
        Some(tl::Node::Tag(child_tag)) => {
            let tag_name = crate::converter::normalized_tag_name(child_tag.name().as_utf8_str());
            match tag_name.as_ref() {
                "rt" => {
                    let mut annotation = String::new();
                    walk_child(child_handle, &mut annotation, ruby_context, handler);
                    flush_base(current_base, handler.output);
                    handler.output.push_str(annotation.trim());
                }
                "rb" => {
                    flush_base(current_base, handler.output);
                    walk_child(child_handle, current_base, ruby_context, handler);
                }
                "rp" => {}
                _ => walk_child(child_handle, current_base, ruby_context, handler),
            }
        }
        Some(tl::Node::Raw(_)) => walk_child(child_handle, current_base, ruby_context, handler),
        _ => {}
    }
}

fn flush_base(current_base: &mut String, output: &mut String) {
    if !current_base.is_empty() {
        output.push_str(current_base.trim());
        current_base.clear();
    }
}

#[derive(Default)]
struct GroupedRuby {
    base_text: String,
    annotations: Vec<String>,
    rtc_content: String,
}

fn render_grouped(tag: &tl::HTMLTag<'_>, ruby_context: &Context, has_rtc: bool, handler: &mut HandlerContext<'_>) {
    let mut grouped = GroupedRuby::default();
    for child_handle in tag.children().top().iter() {
        collect_grouped_child(child_handle, ruby_context, &mut grouped, handler);
    }
    emit_grouped(grouped, has_rtc, handler.output);
}

fn collect_grouped_child(
    child_handle: &tl::NodeHandle,
    ruby_context: &Context,
    grouped: &mut GroupedRuby,
    handler: &HandlerContext<'_>,
) {
    match child_handle.get(handler.parser) {
        Some(tl::Node::Tag(child_tag)) => {
            let tag_name = crate::converter::normalized_tag_name(child_tag.name().as_utf8_str());
            match tag_name.as_ref() {
                "rt" => {
                    let mut annotation = String::new();
                    walk_child(child_handle, &mut annotation, ruby_context, handler);
                    grouped.annotations.push(annotation);
                }
                "rtc" => walk_child(child_handle, &mut grouped.rtc_content, ruby_context, handler),
                "rp" => {}
                _ => walk_child(child_handle, &mut grouped.base_text, ruby_context, handler),
            }
        }
        Some(tl::Node::Raw(_)) => walk_child(child_handle, &mut grouped.base_text, ruby_context, handler),
        _ => {}
    }
}

fn emit_grouped(grouped: GroupedRuby, has_rtc: bool, output: &mut String) {
    output.push_str(grouped.base_text.trim());
    let annotation = grouped.annotations.iter().map(|value| value.trim()).collect::<String>();
    if !annotation.is_empty() {
        if has_rtc && !grouped.rtc_content.trim().is_empty() && grouped.annotations.len() > 1 {
            output.push('(');
            output.push_str(&annotation);
            output.push(')');
        } else {
            output.push_str(&annotation);
        }
    }
    if !grouped.rtc_content.trim().is_empty() {
        output.push_str(grouped.rtc_content.trim());
    }
}

fn handle_base(tag: &tl::HTMLTag<'_>, handler: &mut HandlerContext<'_>) {
    let text_context = handler.context.inline_buffer(handler.output, false);
    let mut text = String::new();
    collect_children(tag, &mut text, &text_context, handler);
    handler.output.push_str(text.trim());
}

fn handle_annotation(tag: &tl::HTMLTag<'_>, handler: &mut HandlerContext<'_>) {
    let text_context = handler.context.inline_buffer(handler.output, true);
    let mut text = String::new();
    collect_children(tag, &mut text, &text_context, handler);
    let trimmed = text.trim();
    if handler.output.ends_with('(') {
        handler.output.push_str(trimmed);
    } else {
        handler.output.push('(');
        handler.output.push_str(trimmed);
        handler.output.push(')');
    }
}

fn handle_parenthesis(tag: &tl::HTMLTag<'_>, handler: &mut HandlerContext<'_>) {
    let content_context = handler.context.inline_buffer(handler.output, false);
    let mut content = String::new();
    collect_children(tag, &mut content, &content_context, handler);
    if !content.trim().is_empty() {
        handler.output.push_str(content.trim());
    }
}

fn walk_children_to_output(tag: &tl::HTMLTag<'_>, handler: &mut HandlerContext<'_>) {
    for child_handle in tag.children().top().iter() {
        crate::converter::walk_node(
            child_handle,
            handler.parser,
            handler.output,
            crate::converter::block::container::HandlerContext::new(
                handler.options,
                handler.context,
                handler.depth + 1,
                handler.dom_context,
            ),
        );
    }
}

fn collect_children(tag: &tl::HTMLTag<'_>, output: &mut String, context: &Context, handler: &HandlerContext<'_>) {
    for child_handle in tag.children().top().iter() {
        walk_child(child_handle, output, context, handler);
    }
}

fn walk_child(child_handle: &tl::NodeHandle, output: &mut String, context: &Context, handler: &HandlerContext<'_>) {
    crate::converter::walk_node(
        child_handle,
        handler.parser,
        output,
        crate::converter::block::container::HandlerContext::new(
            handler.options,
            context,
            handler.depth + 1,
            handler.dom_context,
        ),
    );
}
