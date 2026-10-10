// ~keep reason: visitor API helper functions are pub(crate) surface; not all are called in every
// ~keep build configuration but they are part of the intentional visitor API contract.
#![allow(dead_code)]

//! Visitor state management and context building.
//!
//! This module handles construction of `NodeContext` objects that represent
//! the state of DOM nodes during traversal.

use std::collections::BTreeMap;

use crate::visitor::NodeContext;
use crate::visitor::NodeType;
use std::borrow::Cow;

/// Build a `NodeContext` from current parsing state.
///
/// Creates a complete `NodeContext` suitable for passing to visitor callbacks.
/// This function collects metadata about the current node from various sources:
/// - Tag name and attributes from the HTML element
/// - Depth and parent information from the DOM tree
/// - Index among siblings for positional awareness
/// - Inline/block classification
///
/// # Parameters
///
/// - `node_type`: Coarse-grained classification (Link, Image, Heading, etc.)
/// - `tag_name`: Raw HTML tag name (e.g., "div", "h1", "custom-element")
/// - `attributes`: All HTML attributes as key-value pairs
/// - `depth`: Nesting depth in the DOM tree (0 = root)
/// - `index_in_parent`: Zero-based index among siblings
/// - `parent_tag`: Parent element's tag name (None if root)
/// - `is_inline`: Whether this element is treated as inline vs block
///
/// # Returns
///
/// A fully populated `NodeContext` ready for visitor dispatch.
///
/// # Performance
///
/// This function performs minimal allocations:
/// - Clones `tag_name` (typically 2-10 bytes)
/// - Clones `parent_tag` if present (typically 2-10 bytes)
/// - Clones the attributes `BTreeMap` (heap allocation if non-empty)
///
/// For text nodes and simple elements without attributes, allocations are minimal.
///
/// # Examples
///
/// ```text
/// let ctx = build_node_context(
///     NodeContextParts {
///         node_type: NodeType::Heading,
///         tag_name: "h1",
///         attributes: &attrs,
///         depth: 1,
///         index_in_parent: 0,
///         parent_tag: Some("body"),
///         is_inline: false,
///     },
/// );
/// ```
pub struct NodeContextParts<'a> {
    pub node_type: NodeType,
    pub tag_name: &'a str,
    pub attributes: &'a BTreeMap<String, String>,
    pub depth: usize,
    pub index_in_parent: usize,
    pub parent_tag: Option<&'a str>,
    pub is_inline: bool,
}

#[inline]
pub fn build_node_context(parts: NodeContextParts<'_>) -> NodeContext<'_> {
    NodeContext::with_borrowed_attributes(
        parts.node_type,
        Cow::Borrowed(parts.tag_name),
        parts.attributes,
        parts.depth,
        parts.index_in_parent,
        parts.parent_tag.map(Cow::Borrowed),
        parts.is_inline,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_node_context() {
        let mut attrs = BTreeMap::new();
        attrs.insert("id".to_string(), "main".to_string());
        attrs.insert("class".to_string(), "container".to_string());

        let ctx = build_node_context(NodeContextParts {
            node_type: NodeType::Div,
            tag_name: "div",
            attributes: &attrs,
            depth: 2,
            index_in_parent: 3,
            parent_tag: Some("body"),
            is_inline: false,
        });

        assert_eq!(ctx.node_type, NodeType::Div);
        assert_eq!(ctx.tag_name, "div");
        assert_eq!(ctx.depth, 2);
        assert_eq!(ctx.index_in_parent, 3);
        assert_eq!(ctx.parent_tag.as_deref(), Some("body"));
        assert!(!ctx.is_inline);
        assert_eq!(ctx.attributes().len(), 2);
        assert_eq!(ctx.attributes().get("id"), Some(&"main".to_string()));
    }

    #[test]
    fn test_build_node_context_no_parent() {
        let attrs = BTreeMap::new();

        let ctx = build_node_context(NodeContextParts {
            node_type: NodeType::Html,
            tag_name: "html",
            attributes: &attrs,
            depth: 0,
            index_in_parent: 0,
            parent_tag: None,
            is_inline: false,
        });

        assert_eq!(ctx.node_type, NodeType::Html);
        assert_eq!(ctx.parent_tag, None);
        assert!(ctx.attributes().is_empty());
    }
}
