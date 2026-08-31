#[cfg(not(feature = "extra-platforms"))]
use alloc::sync::Arc;
#[cfg(feature = "extra-platforms")]
use portable_atomic_util::Arc;

use crate::context;
use crate::error::EndOrError;

/// Type alias for results returned by I/O-less parsers.
pub type Result<T> = core::result::Result<T, EndOrError>;

/// Configure how to treat XML comments encountered during parsing.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum CommentMode {
	/// Comments raise a
	/// [`Error::RestrictedXml`][`crate::error::Error::RestrictedXml`] error.
	///
	/// This is the default mode.
	#[default]
	Reject,

	/// Comments are silently ignored.
	Discard,
}

/**
Parser configuration
*/
#[derive(Debug)]
pub struct Options {
	/// Maximum number of bytes which can form a token.
	///
	/// This limits the number of bytes in an attribute value, attribute name,
	/// element name or text chunk.
	///
	/// If a text chunk exceeds this size, it is split and emitted as separate
	/// text events.
	///
	/// If an element name, attribute name, or attribute value exceeds this
	/// length, a
	/// [`rxml::Error::RestrictedXml`][`crate::Error::RestrictedXml`] error is
	/// returned.
	pub max_token_length: usize,

	/// Parser context to use.
	pub context: Option<Arc<context::Context>>,

	/// Allow the use of comments.
	pub comments: CommentMode,
}

impl Clone for Options {
	fn clone(&self) -> Self {
		Self {
			max_token_length: self.max_token_length,
			context: self.context.as_ref().map(Arc::clone),
			comments: self.comments,
		}
	}
}

impl From<Options> for crate::lexer::LexerOptions {
	fn from(other: Options) -> Self {
		Self {
			max_token_length: other.max_token_length,
		}
	}
}

impl From<&Options> for crate::lexer::LexerOptions {
	fn from(other: &Options) -> Self {
		Self {
			max_token_length: other.max_token_length,
		}
	}
}

impl From<Options> for super::raw::RawOptions {
	fn from(other: Options) -> Self {
		Self {
			max_token_length: other.max_token_length,
			comments: other.comments,
		}
	}
}

impl From<&Options> for super::raw::RawOptions {
	fn from(other: &Options) -> Self {
		Self {
			max_token_length: other.max_token_length,
			comments: other.comments,
		}
	}
}

impl Default for Options {
	/// Constructs default options.
	///
	/// The defaults are implementation-defined and should not be relied upon.
	fn default() -> Self {
		Self {
			max_token_length: 8192,
			context: None,
			comments: CommentMode::default(),
		}
	}
}

/**
# XML version number

Only version 1.0 is supported.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlVersion {
	/// XML Version 1.0
	V1_0,
}

/// XML core namespace URI (for the `xml:` prefix)
pub const XMLNS_XML: &str = "http://www.w3.org/XML/1998/namespace";
/// XML namespace URI (for the `xmlns:` prefix)
pub const XMLNS_XMLNS: &str = "http://www.w3.org/2000/xmlns/";
/// Empty namespace URI
pub(crate) const XMLNS_UNNAMESPACED: &str = "";

/// Carry measurement information about the event
///
/// Events are always consecutive. As a caveat, any whitespace between the XML
/// declaration and the root element is attributed to the root element header.
/// While it would, semantically, make more sense to attribute it to the XML
/// declaration, this is difficult to achieve. This behaviour may change.
///
/// Event length overflows are reported as [`Error::RestrictedXml`] errors.
///
///   [`Error::RestrictedXml`]: crate::Error::RestrictedXml
#[derive(Copy, Debug, Clone, PartialEq, Eq)]
pub struct EventMetrics {
	pub(super) len: usize,
}

impl EventMetrics {
	/// Return whether this event used no bytes.
	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Get the number of bytes used to generate this event.
	pub fn len(&self) -> usize {
		self.len
	}

	/// Create new event metrics
	pub const fn new(len: usize) -> EventMetrics {
		EventMetrics { len }
	}

	/// Create new event metrics of length 0.
	pub const fn zero() -> Self {
		EventMetrics::new(0)
	}
}

/**
# XML parser trait

This trait represents a parser which can convert bytes into events
representing an XML document.

It is implemented for [`Parser`][`crate::Parser`] (with
[`Event`][`crate::Event`]) and
[`RawParser`][`crate::RawParser`] (with [`RawEvent`][`crate::RawEvent`]).
*/
pub trait Parse {
	/// The type of XML event which is emitted by the parser.
	type Output;

	/// Parse a single event from the bytes in `buf`.
	///
	/// `at_eof` signals to the parser whether `buf` contains the entire
	/// remainder of the document. If `at_eof` is false, the parser will
	/// return a [`WouldBlock`][`std::io::ErrorKind::WouldBlock`] I/O error
	/// when it reaches the end of the buffer.
	///
	/// If the end of file has been reached after parsing a valid document,
	/// `None` is returned. Otherwise, if the document is still acceptable the
	/// next XML event is returned.
	///
	/// If the document violates a constraint, such as the XML 1.0
	/// grammar or namespacing rules, the corresponding error is returned.
	///
	/// **Note:** Parsing may emit more than one event even for a single byte,
	/// which is why this function should be called until it returns
	/// [`WouldBlock`][`std::io::ErrorKind::WouldBlock`] or `None` on any
	/// given buffer.
	fn parse(&mut self, buf: &mut &[u8], at_eof: bool) -> Result<Option<Self::Output>>;

	/// Release all temporary buffers or other ephemeral allocations
	///
	/// This is sensible to call when it is expected that no more data will be
	/// processed by the parser for a while and the memory is better used
	/// elsewhere.
	fn release_temporaries(&mut self);

	/// Parse all data from the given buffer and pass the generated events to
	/// a callback.
	///
	/// In contrast to [`parse`][`Self::parse`], on success, this always
	/// consumes the entire buffer. Events which are encountered while
	/// processing the buffer are handed to the given callback.
	///
	/// The end-of-file behaviour is identical to `parse`.
	///
	/// See also [`as_eof_flag`][`crate::as_eof_flag`] to convert a
	/// `WouldBlock` error to a boolean.
	fn parse_all<F: FnMut(Self::Output)>(
		&mut self,
		data: &mut &[u8],
		at_eof: bool,
		mut f: F,
	) -> Result<()> {
		loop {
			match self.parse(data, at_eof)? {
				None => return Ok(()),
				Some(ev) => f(ev),
			}
		}
	}

	/// Parse a [`bytes::Buf`].
	///
	/// This is a wrapper around [`parse`][`Self::parse`], handling the edge
	/// cases of chunked buffers combined with `at_eof` correctly.
	///
	/// For further information about parsing, please see
	/// [`parse`][`Self::parse`].
	fn parse_buf<T: bytes::Buf>(
		&mut self,
		buf: &mut T,
		at_eof: bool,
	) -> Result<Option<Self::Output>> {
		loop {
			let mut chunk = buf.chunk();
			let init_len = chunk.len();
			// Only consider eof if the current chunk is truly the last one, which can be determined by checking that the chunk contains all remaining bytes.
			let at_eof = at_eof && init_len == buf.remaining();
			let result = self.parse(&mut chunk, at_eof);
			let new_len = chunk.len();
			let consumed = init_len - new_len;
			buf.advance(consumed);
			match result {
				Err(EndOrError::NeedMoreData) => {
					if buf.remaining() > 0 {
						assert!(consumed > 0);
						continue;
					} else {
						return Err(EndOrError::NeedMoreData);
					}
				}
				other => return other,
			}
		}
	}

	/// Parse a [`bytes::Buf`] completely.
	///
	/// This is a wrapper around [`parse`][`Self::parse`], using the same
	/// logic as [`parse_all`][`Self::parse_all`], handling the edge cases of
	/// chunked buffers combined with `at_eof` correctly.
	///
	/// For further information about parsing, please see
	/// [`parse`][`Self::parse`].
	fn parse_all_buf<T: bytes::Buf, F: FnMut(Self::Output)>(
		&mut self,
		buf: &mut T,
		at_eof: bool,
		mut f: F,
	) -> Result<()> {
		loop {
			match self.parse_buf(buf, at_eof)? {
				None => return Ok(()),
				Some(ev) => f(ev),
			}
		}
	}
}

/**
Trait for things which can be constructed with [`Options`].
*/
pub trait WithOptions {
	/// Create a new instance using the given options.
	fn with_options(options: Options) -> Self;
}
