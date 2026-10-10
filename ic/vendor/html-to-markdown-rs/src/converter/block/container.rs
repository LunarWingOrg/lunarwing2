//! Handler for structural container elements.
//!
//! This module provides handlers for structural containers that process their
//! children without special formatting or whitespace truncation:
//! - body, html: Structural document containers
//! - time, data: Inline semantic containers
//! - thead, tbody, tfoot, tr, th, td: Table structure (handled elsewhere)
//! - source: Media source element
//! - wbr: Word break opportunity (no-op)

use crate::options::ConversionOptions;
use tl::{NodeHandle, Parser};

type Context = crate::converter::Context;
type DomContext = crate::converter::DomContext;

#[derive(Clone, Copy)]
pub struct HandlerContext<'a> {
    pub options: &'a ConversionOptions,
    pub ctx: &'a Context,
    pub depth: usize,
    pub dom_ctx: &'a DomContext,
}

impl<'a> HandlerContext<'a> {
    pub const fn new(options: &'a ConversionOptions, ctx: &'a Context, depth: usize, dom_ctx: &'a DomContext) -> Self {
        Self {
            options,
            ctx,
            depth,
            dom_ctx,
        }
    }
}

/// Handle structural container elements that recursively process children.
///
/// This is used for elements like `body` and `html` that should process their
/// children directly without any whitespace truncation or special formatting.
///
/// # Arguments
/// * `node_handle` - Handle to the HTML node
/// * `parser` - The HTML parser
/// * `output` - Accumulation buffer for Markdown output
/// * `options` - Conversion options
/// * `ctx` - Current conversion context
/// * `depth` - Current recursion depth
/// * `dom_ctx` - DOM context for tracking relationships
pub fn handle_structural_container(
    node_handle: &NodeHandle,
    parser: &Parser,
    output: &mut String,
    handler: HandlerContext<'_>,
) {
    let Some(node) = node_handle.get(parser) else {
        return;
    };

    let tl::Node::Tag(tag) = node else {
        return;
    };

    let children = tag.children();
    for child_handle in children.top().iter() {
        crate::converter::main::walk_node(
            child_handle,
            parser,
            output,
            crate::converter::block::container::HandlerContext::new(
                handler.options,
                handler.ctx,
                handler.depth + 1,
                handler.dom_ctx,
            ),
        );
    }
}

/// Handle pass-through container elements that process children inline.
///
/// This is used for semantic elements like `time` and `data` that wrap content
/// but should not add any additional formatting or block breaks.
///
/// # Arguments
/// * `node_handle` - Handle to the HTML node
/// * `parser` - The HTML parser
/// * `output` - Accumulation buffer for Markdown output
/// * `options` - Conversion options
/// * `ctx` - Current conversion context
/// * `depth` - Current recursion depth
/// * `dom_ctx` - DOM context for tracking relationships
pub fn handle_passthrough(node_handle: &NodeHandle, parser: &Parser, output: &mut String, handler: HandlerContext<'_>) {
    let Some(node) = node_handle.get(parser) else {
        return;
    };

    let tl::Node::Tag(tag) = node else {
        return;
    };

    let children = tag.children();
    for child_handle in children.top().iter() {
        crate::converter::main::walk_node(
            child_handle,
            parser,
            output,
            crate::converter::block::container::HandlerContext::new(
                handler.options,
                handler.ctx,
                handler.depth + 1,
                handler.dom_ctx,
            ),
        );
    }
}

/// Handle no-op container elements that should be ignored.
///
/// This is used for elements like `wbr` (word break opportunity) and `source`
/// (media source specification) that should not produce any output.
///
/// # Arguments
/// * `_node_handle` - Handle to the HTML node (unused)
/// * `_parser` - The HTML parser (unused)
/// * `_output` - Accumulation buffer for Markdown output (unused)
/// * `_options` - Conversion options (unused)
/// * `_ctx` - Current conversion context (unused)
/// * `_depth` - Current recursion depth (unused)
/// * `_dom_ctx` - DOM context (unused)
#[inline]
pub const fn handle_noop() {}
