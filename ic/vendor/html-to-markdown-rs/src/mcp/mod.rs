//! Model Context Protocol (MCP) server implementation.
//!
//! Provides an MCP server that exposes html-to-markdown conversion across the
//! tools, prompts, resources, and completions capabilities.
//!
//! # Tools
//!
//! - **convert_html**: Convert HTML to Markdown (or Djot/plain) with typed, fully
//!   discoverable `ConvertConfig` options; `json:true` returns the full `ConversionResult`.
//! - **extract_metadata**: Extract structured `<head>`/`<meta>` metadata (title, Open Graph,
//!   Twitter Card, JSON-LD, headers, links, images) as JSON.
//!
//! # Prompts
//!
//! - **convert_to_markdown**, **extract_main_content**, **inspect_metadata**: ready-made
//!   workflow templates that drive the tools.
//!
//! # Resources
//!
//! - **htmltomarkdown://options-schema**: JSON Schema of every conversion option.
//! - **htmltomarkdown://output-formats**: guide to the markdown/djot/plain formats.
//!
//! Argument completions are provided for prompt arguments (e.g. `output_format`).
//!
//! # Example
//!
//! ```rust,no_run
//! use html_to_markdown_rs::mcp::start_mcp_server;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//!     start_mcp_server().await?;
//!     Ok(())
//! }
//! ```

mod catalog;
mod errors;
mod format;
mod params;
mod server;

pub use server::start_mcp_server;
#[cfg(feature = "mcp-http")]
pub use server::start_mcp_server_http;
