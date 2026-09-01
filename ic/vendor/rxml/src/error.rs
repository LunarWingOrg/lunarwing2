/*!
# Error types

This module holds the error types returned by the various functions of this
crate.
*/
use core::error;
use core::fmt;
use core::result::Result as StdResult;

use rxml_validation::Error as ValidationError;

/// Parser or lexer context in which an error occured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorContext {
	/// During processing of text content outside of a CDATA section.
	Text,

	/// During processing of an attribute value.
	AttributeValue,

	/// During processing of an element or attribute name.
	Name,

	/// During processing of an attribute name.
	AttributeName,

	/// At the start of an element or attribute name.
	NameStart,

	/// During processing of an element.
	Element,

	/// During processing of an element footer.
	ElementFoot,

	/// During processing of an element's closing part.
	ElementClose,

	/// During processing of text content inside a CDATA section.
	CdataSection,

	/// During processing of a CDATA section's start.
	CdataSectionStart,

	/// During processing of the XML declaration.
	XmlDeclaration,

	/// During processing of the start of an XML declaration.
	XmlDeclarationStart,

	/// During processing of the end of an XML declaration.
	XmlDeclarationEnd,

	/// During processing of an entity or character reference.
	Reference,

	/// At the beginning of the document.
	DocumentBegin,

	/// After the end of the root element.
	DocumentEnd,

	/// Inside comment.
	Comment,
}

impl ErrorContext {
	fn as_str(self) -> &'static str {
		match self {
			Self::Text => "in text node",
			Self::AttributeValue => "in attribute value",
			Self::Name => "in name",
			Self::AttributeName => "in attribute name",
			Self::NameStart => "at start of name",
			Self::Element => "in element",
			Self::ElementFoot => "in element footer",
			Self::ElementClose => "at element close",
			Self::CdataSection => "in CDATA section",
			Self::CdataSectionStart => "at CDATA section marker",
			Self::XmlDeclaration => "in XML declaration",
			Self::XmlDeclarationStart => "at start of XML declaration",
			Self::XmlDeclarationEnd => "at end of XML declaration",
			Self::Reference => "in entity or character reference",
			Self::DocumentBegin => "at beginning of document",
			Self::DocumentEnd => "at end of document",
			Self::Comment => "inside comment",
		}
	}
}

fn ctx_as_str(ctx: &Option<ErrorContext>) -> &'static str {
	ctx.map(|ctx| ctx.as_str()).unwrap_or("in unknown context")
}

impl fmt::Display for ErrorContext {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

/// Error types which may be returned from functions in this crate.
///
/// For parsing and lexing, all errors are fatal and will be returned
/// indefinitely from the parser after the first encounter.
///
/// Most errors deal with violations of a well-formedness or
/// namespace-well-formedness constraint or the XML 1.0 grammar.
#[derive(Debug, Clone, PartialEq, Copy)]
pub enum Error {
	/// End-of-file encountered during a construct where more data was
	/// expected.
	///
	/// The contents are implementation details.
	InvalidEof(Option<ErrorContext>),

	/// Attempt to refer to an undeclared entity.
	///
	/// **Note**: May also be emitted in some cases of malformed entities as
	/// the lexer is very conservative about how many chars are read to
	/// interpret an entity.
	UndeclaredEntity,

	/// Unicode codepoint which is not allowed in XML 1.0 encountered.
	///
	/// The contents are implementation details.
	InvalidChar(Option<ErrorContext>, u32, bool),

	/// Unicode codepoint which was not expected at that point in the
	/// grammar.
	///
	/// The contents are implementation details.
	UnexpectedChar(Option<ErrorContext>, char, Option<&'static [&'static str]>),

	/// Byte which was not expected at that point in the grammar.
	///
	/// The contents are implementation details.
	UnexpectedByte(Option<ErrorContext>, u8, Option<&'static [&'static str]>),

	/// Generalized invalid syntactic construct which does not fit into any
	/// of the other categories.
	///
	/// The contents are implementation details.
	InvalidSyntax(&'static str),

	/// Token was not expected by the parser at that point in the grammar.
	///
	/// The contents are implementation details.
	UnexpectedToken(
		Option<ErrorContext>,
		&'static str,
		Option<&'static [&'static str]>,
	),

	/// Attribute was declared multiple times in the same element.
	///
	/// **Note:** This will also be emitted for namespaced attributes which
	/// resolve to the same `(uri, localname)` pair after prefix resolution.
	DuplicateAttribute,

	/// Ending tag name does not match opening tag.
	ElementMismatch,

	/// More than one colon encountered in a name.
	///
	/// The contents are implementation details.
	MultiColonName(Option<ErrorContext>),

	/// One side of the colon in a name was empty.
	///
	/// The contents are implementation details.
	EmptyNamePart(Option<ErrorContext>),

	/// Use of an undeclared namespace prefix.
	///
	/// The contents are implementation details.
	UndeclaredNamespacePrefix(Option<ErrorContext>),

	/// Attempt to redefine a reserved namespace prefix.
	ReservedNamespacePrefix,

	/// Attempt to bind a reserved namespace name to the wrong prefix.
	ReservedNamespaceName,

	/// Local name does not conform to Name production (invalid start char)
	InvalidLocalName(Option<ErrorContext>),

	/// Declared namespace URI is empty
	EmptyNamespaceUri,

	/// An invalid UTF-8 byte was encountered during decoding.
	InvalidUtf8Byte(u8),

	/// A forbidden construct was encountered during lexing or parsing.
	///
	/// The string indicates the context and should not be interpreted by user
	/// code.
	RestrictedXml(&'static str),
}

impl Error {
	pub(crate) fn from_validation(e: ValidationError, ctx: Option<ErrorContext>) -> Self {
		match e {
			ValidationError::EmptyName => Self::InvalidSyntax("empty name"),
			ValidationError::InvalidChar(ch) => Self::UnexpectedChar(ctx, ch, None),
			ValidationError::EmptyNamePart => Self::EmptyNamePart(ctx),
			ValidationError::MultiColonName => Self::MultiColonName(ctx),
			ValidationError::InvalidLocalName => Self::InvalidLocalName(ctx),
		}
	}

	pub(crate) fn utf8err(src: &[u8], e: &core::str::Utf8Error) -> Error {
		Error::InvalidUtf8Byte(src[e.valid_up_to()])
	}
}

impl error::Error for Error {}

impl ErrorWithContext for Error {
	fn with_context(self, ctx: ErrorContext) -> Self {
		let ctx = Some(ctx);
		match self {
			Self::InvalidEof(_) => Self::InvalidEof(ctx),
			Self::InvalidChar(_, cp, fromref) => Self::InvalidChar(ctx, cp, fromref),
			Self::UnexpectedChar(_, ch, alt) => Self::UnexpectedChar(ctx, ch, alt),
			Self::UnexpectedToken(_, tok, alt) => Self::UnexpectedToken(ctx, tok, alt),
			Self::MultiColonName(_) => Self::MultiColonName(ctx),
			Self::EmptyNamePart(_) => Self::EmptyNamePart(ctx),
			Self::UndeclaredNamespacePrefix(_) => Self::UndeclaredNamespacePrefix(ctx),
			Self::InvalidLocalName(_) => Self::InvalidLocalName(ctx),
			other => other,
		}
	}
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			Self::InvalidEof(ctx) => write!(f, "invalid eof {}", ctx_as_str(ctx)),
			Self::UndeclaredEntity => write!(f, "use of undeclared entity"),
			Self::InvalidChar(ctx, cp, false) => {
				write!(f, "invalid codepoint U+{:x} {}", cp, ctx_as_str(ctx))
			}
			Self::InvalidChar(ctx, cp, true) => write!(
				f,
				"character reference expanded to invalid codepoint U+{:x} {}",
				cp,
				ctx_as_str(ctx)
			),
			Self::UnexpectedChar(ctx, ch, Some(opts)) if !opts.is_empty() => {
				write!(
					f,
					"U+{:x} not allowed {} (expected ",
					*ch as u32,
					ctx_as_str(ctx)
				)?;
				if opts.len() == 1 {
					f.write_str(opts[0])?;
					f.write_str(")")
				} else {
					f.write_str("one of: ")?;
					for (i, opt) in opts.iter().enumerate() {
						if i > 0 {
							f.write_str(", ")?;
						}
						f.write_str(opt)?;
					}
					f.write_str(")")
				}
			}
			Self::UnexpectedByte(ctx, b, Some(opts)) if !opts.is_empty() => {
				write!(f, "0x{:x} not allowed {} (expected ", *b, ctx_as_str(ctx))?;
				if opts.len() == 1 {
					f.write_str(opts[0])?;
					f.write_str(")")
				} else {
					f.write_str("one of: ")?;
					for (i, opt) in opts.iter().enumerate() {
						if i > 0 {
							f.write_str(", ")?;
						}
						f.write_str(opt)?;
					}
					f.write_str(")")
				}
			}
			Self::UnexpectedChar(ctx, ch, _) => {
				write!(f, "U+{:x} not allowed {}", *ch as u32, ctx_as_str(ctx))
			}
			Self::UnexpectedByte(ctx, b, _) => {
				write!(f, "0x{:x} not allowed {}", *b, ctx_as_str(ctx))
			}
			Self::InvalidSyntax(msg) => write!(f, "invalid syntax: {}", msg),
			Self::UnexpectedToken(ctx, tok, Some(opts)) if !opts.is_empty() => {
				write!(f, "unexpected {} token {} (expected ", tok, ctx_as_str(ctx))?;
				if opts.len() == 1 {
					f.write_str(opts[0])?;
					f.write_str(")")
				} else {
					f.write_str("one of: ")?;
					for (i, opt) in opts.iter().enumerate() {
						if i > 0 {
							f.write_str(", ")?;
						}
						f.write_str(opt)?;
					}
					f.write_str(")")
				}
			}
			Self::UnexpectedToken(ctx, tok, _) => {
				write!(f, "unexpected {} token {}", tok, ctx_as_str(ctx))
			}
			Self::DuplicateAttribute => f.write_str("duplicate attribute"),
			Self::ElementMismatch => f.write_str("start and end tag do not match"),
			Self::MultiColonName(ctx) => write!(f, "more than one colon {} name", ctx_as_str(ctx)),
			Self::EmptyNamePart(ctx) => {
				write!(
					f,
					"empty string on one side of the colon {} name",
					ctx_as_str(ctx)
				)
			}
			Self::UndeclaredNamespacePrefix(ctx) => {
				write!(
					f,
					"use of undeclared namespace prefix {} name",
					ctx_as_str(ctx)
				)
			}
			Self::ReservedNamespacePrefix => f.write_str("reserved namespace prefix"),
			Self::ReservedNamespaceName => f.write_str("reserved namespace URI"),
			Self::InvalidLocalName(ctx) => {
				write!(f, "local name is invalid {} name", ctx_as_str(ctx))
			}
			Self::EmptyNamespaceUri => write!(f, "namespace URI is empty"),
			Self::RestrictedXml(msg) => write!(f, "restricted xml: {}", msg),
			Self::InvalidUtf8Byte(b) => write!(f, "invalid utf-8 byte: \\x{:02x}", b),
		}
	}
}

impl From<ValidationError> for Error {
	fn from(other: ValidationError) -> Self {
		Self::from_validation(other, None)
	}
}

/// The default result type for this crate.
pub type Result<T> = StdResult<T, Error>;

pub(crate) trait ErrorWithContext {
	fn with_context(self, ctx: ErrorContext) -> Self;
}

impl Error {
	pub(crate) fn wfeof(ctx: ErrorContext) -> Error {
		Self::InvalidEof(Some(ctx))
	}
}

pub(crate) fn add_context<T, E: ErrorWithContext>(
	r: StdResult<T, E>,
	ctx: ErrorContext,
) -> StdResult<T, E> {
	r.map_err(|e| e.with_context(ctx))
}

/// Error type which indicates either a real error or the need for more data
/// to continue parsing.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub enum EndOrError {
	/// More data is needed.
	NeedMoreData,

	/// A fatal parsing error occurred.
	Error(Error),
}

impl ErrorWithContext for EndOrError {
	fn with_context(self, ctx: ErrorContext) -> Self {
		match self {
			Self::NeedMoreData => Self::NeedMoreData,
			Self::Error(e) => Self::Error(e.with_context(ctx)),
		}
	}
}

impl From<Error> for EndOrError {
	fn from(other: Error) -> Self {
		Self::Error(other)
	}
}

impl From<ValidationError> for EndOrError {
	fn from(other: ValidationError) -> Self {
		Self::Error(other.into())
	}
}
