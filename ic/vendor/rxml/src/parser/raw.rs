/*!
# Restricted XML 1.0 Parser, sans namespacing
*/
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::error::*;
use crate::lexer::{Lexer, Token, TokenMetrics};
use crate::strings::{Name, NcName};

use super::common::{Result, *};

/// This trait exists mainly for testability purposes.
trait TokenRead {
	fn read(&mut self) -> Result<Option<Token>>;
}

struct ByteTokenReader<'x, 'y> {
	lexer: &'x mut Lexer,
	// one of the rare cases where you actually need two different lifetimes,
	// otherwise the bounds on 'x become too strict (wide), making it
	// impossible to use sensibly.
	buf: &'x mut &'y [u8],
	at_eof: bool,
}

impl TokenRead for ByteTokenReader<'_, '_> {
	fn read(&mut self) -> Result<Option<Token>> {
		self.lexer.lex_bytes(self.buf, self.at_eof)
	}
}

/// Pair of an optional namespace prefix and a localpart, commonly used in
/// element and attribute names.
pub type RawQName = (Option<NcName>, NcName);

/**
# Logical XML document parts

The term *Event* is borrowed from SAX terminology. Each [`RawEvent`] refers to
a logical bit of the XML document which has been parsed.

Note that observing a [`RawEvent`] **does not imply that the document has been
well-formed or namespace-well-formed** up to this point. See [`RawParser`] for
caveats.

Each event has [`EventMetrics`] attached which give information about the
number of bytes from the input stream used to generate the event.

## Document event sequence

A well-formed XML document will generate the following sequence of events:

1. Zero or one [`Self::XmlDeclaration`]
2. One *element sequence*

An *element sequence* consists of:

1. [`Self::ElementHeadOpen`]
2. Zero or more [`Self::Attribute`]
3. [`Self::ElementHeadClose`]
4. Zero or more element sequences or [`Self::Text`], mixed arbitrarily
5. [`Self::ElementFoot`]
*/
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RawEvent {
	/// The XML declaration.
	///
	/// As the `encoding` and `standalone` flag are forced to be `utf-8` and
	/// `yes` respectively (or absent), those values are not emitted.
	XmlDeclaration(
		/// Number of bytes contributing to this event.
		///
		/// This includes all bytes from the opening `<?` until and including
		/// the closing `?>`.
		EventMetrics,
		/// XML version number
		XmlVersion,
	),

	/// Start of an XML element header
	ElementHeadOpen(
		/// Number of bytes contributing to this event.
		///
		/// This includes the opening `<` as well as the element name. If this
		/// is the root element, any whitespace between the XML declaration
		/// and the opening `<` of the root element is also incldued.
		EventMetrics,
		/// Prefix/localpart pair of the element.
		RawQName,
	),

	/// Attribute key/value pair
	///
	/// Note that in raw events, XML namespace declarations are just
	/// attributes, as no namespace resolution takes place.
	///
	/// However, the following local constraints are still enforced:
	/// - [Namespace constraint: Reserved Prefixes and Namespace Names](https://www.w3.org/TR/REC-xml-names/#xmlReserved):
	///   The [`XMLNS_XML`] namespace name may only be bound to `xmlns:xml`,
	///   and only that name is allowed for the `xmlns:xml` prefix declartion.
	///
	///   The `xmlns` prefix can never be bound.
	/// - [Namespacing constraint: No Prefix Undeclaring](https://www.w3.org/TR/REC-xml-names/#nsc-NoPrefixUndecl):
	///   Attributes with the prefix `xmlns` (and any localname) with an empty
	///   value are rejected. However, as per
	///   [§6.2 of Namespaces in XML 1.0](https://www.w3.org/TR/REC-xml-names/#defaulting),
	///   the `xmlns` attribute (without prefix) *is* valid with an empty
	///   value, indicating the undeclaring of the default namespace.
	Attribute(
		/// Number of bytes contributing to this event.
		///
		/// This includes the attribute name, the equal sign as well as the
		/// raw input bytes of the attribute value (pre character entity
		/// expansion). It also includes any whitespace preceding the
		/// attribute name.
		EventMetrics,
		/// Prefix/localpart pair of the attribute name.
		RawQName,
		/// Normalized attribute value
		String,
	),

	/// End of an XML element header
	ElementHeadClose(
		/// Number of bytes contributing to this event.
		///
		/// This includes any whitespace preceding the `>` or `/>`.
		EventMetrics,
	),

	/// The end of an XML element.
	///
	/// The parser enforces proper nesting of the elements, so no additional
	/// information is required.
	ElementFoot(
		/// Number of bytes contributing to this event.
		///
		/// The number of bytes may be zero if this event is emitted in
		/// response to a `/>` in an element header, because the bytes for
		/// `/>` are accounted for in the corresponding
		/// [`Self::ElementHeadClose`].
		EventMetrics,
	),

	/// Text CData.
	///
	/// **Note:** Multiple consecutive `Text` events may be emitted for long
	/// sections of text or because of implementation details in the
	/// processing.
	Text(
		/// Number of bytes contributing to this event.
		///
		/// Note that due to the expansion of character references and the
		/// processing of CDATA sections, the number of bytes consumed will
		/// generally be not equal to the number of bytes in emitted.
		EventMetrics,
		/// Text content
		///
		/// References are expanded and CDATA sections processed correctly, so
		/// that the text in the event exactly corresponds to the *logical*
		/// character data.
		String,
	),
}

impl RawEvent {
	/// Return the [`EventMetrics`] of the event
	pub fn metrics(&self) -> &EventMetrics {
		match self {
			Self::XmlDeclaration(m, ..) => m,
			Self::ElementHeadOpen(m, ..) => m,
			Self::Attribute(m, ..) => m,
			Self::ElementHeadClose(m, ..) => m,
			Self::ElementFoot(m, ..) => m,
			Self::Text(m, ..) => m,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DeclSt {
	VersionName,
	VersionEq,
	VersionValue,
	EncodingName,
	EncodingEq,
	EncodingValue,
	StandaloneName,
	StandaloneEq,
	StandaloneValue,
	Close,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ElementSt {
	// Element opener is expected here, but nothing has been done yet
	Expected,
	AttrName,
	AttrEq,
	AttrValue,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DocSt {
	Element(ElementSt),
	CData,
	ElementFoot,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum CommentReturn {
	Element,
	CData,
}

impl From<CommentReturn> for State {
	fn from(other: CommentReturn) -> Self {
		match other {
			CommentReturn::Element => State::Document(DocSt::Element(ElementSt::Expected)),
			CommentReturn::CData => State::Document(DocSt::CData),
		}
	}
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
	Initial,
	Decl {
		substate: DeclSt,
		version: Option<XmlVersion>,
	},
	Document(DocSt),
	Comment(CommentReturn),
	End,
	Eof,
}

/**
Raw parser configuration
*/
#[derive(Debug)]
pub struct RawOptions {
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

	/// Allow the use of comments.
	pub comments: CommentMode,
}

impl Default for RawOptions {
	fn default() -> Self {
		Options::default().into()
	}
}

impl From<RawOptions> for crate::lexer::LexerOptions {
	fn from(other: RawOptions) -> Self {
		Self {
			max_token_length: other.max_token_length,
		}
	}
}

impl From<&RawOptions> for crate::lexer::LexerOptions {
	fn from(other: &RawOptions) -> Self {
		Self {
			max_token_length: other.max_token_length,
		}
	}
}

/**
# Low-level restricted XML 1.0 parser (without namespace support)

This parser emits [`RawEvent`] items, which represent the logical structure
of the XML document before resolution of namespaces. If you want to use
namespaces and have them resolved fully, use [`Parser`] instead.

## Caveats

It is possible for an XML document to pass parsing using just this parser and
still be not well-formed or namespace-well-formed. In particular, the
following cases are not detected and must be handled by consumer code:

- Duplicate attributes
- Uses of undeclared prefixes
- Multiple attributes resolving to the same namespace URI / localpart pair
  after prefix expansion

See [`Parser`] for a full parser implementation which ensures well-formedness
and namespace-well-formedness.

## Example

```
use rxml::{RawParser, Parse, Error, RawEvent, XmlVersion, error::EndOrError};
use std::io;
let doc = b"<?xml version='1.0'?><hello>World!</hello>";
let mut fp = RawParser::new();
// We expect a NeedMoreData, because the XML declaration is not complete yet
assert!(matches!(
	fp.parse(&mut &doc[..10], false).err().unwrap(),
	EndOrError::NeedMoreData,
));

// Now we pass the XML declaration (and some), so we expect a corresponding
// event
let ev = fp.parse(&mut &doc[10..25], false);
assert!(matches!(ev.unwrap().unwrap(), RawEvent::XmlDeclaration(_, XmlVersion::V1_0)));
```

   [`Parser`]: crate::Parser
*/
#[derive(Debug)]
pub struct RawParser {
	inner: InnerParser,
	lexer: Lexer,
}

impl RawParser {
	/// Create a new parser
	pub fn new() -> Self {
		Self::with_options(Options::default().into())
	}

	pub(super) fn with_options(options: RawOptions) -> Self {
		Self {
			inner: InnerParser::new(options.comments),
			lexer: Lexer::new(options.into()),
		}
	}

	/// Configure text buffering (enabled by default).
	///
	/// If enabled, text content is buffered up to the configured token size
	/// limit, unless it is more efficient to flush it out anyway.
	///
	/// If disabled, text content is emitted as event as soon as at least one
	/// valid char has been read.
	///
	/// Enabling text buffering reduces the number of calls which need to be
	/// made into the parser and thus may improve performance. However, it
	/// also makes the application see the text content later, which may be
	/// problematic if control flow which affects parsing depends on text
	/// content.
	#[inline(always)]
	pub fn set_text_buffering(&mut self, enabled: bool) {
		self.lexer.text_buffering = enabled;
	}

	/// Return whether text buffering is enabled.
	///
	/// See [`set_text_buffering`][`Self::set_text_buffering`]..
	#[inline(always)]
	pub fn text_buffering(&self) -> bool {
		self.lexer.text_buffering
	}
}

impl Default for RawParser {
	fn default() -> Self {
		Self::new()
	}
}

impl WithOptions for RawParser {
	fn with_options(options: Options) -> Self {
		Self::with_options(options.into())
	}
}

struct InnerParser {
	state: State,
	element_stack: Vec<Name>,
	attribute_scratchpad: Option<RawQName>,
	/// end position of the last token processed in the event
	event_last_token_end: Option<usize>,
	/// current length of the event
	event_length: usize,
	/// Non-emitted pre-event content. This is used only for comments and
	/// added to the `event_length` when a new event starts.
	pre_event_length: usize,
	/// Internal queue for events which will be returned from the current
	/// and potentially future calls to `parse()`.
	///
	/// In contrast to the Lexer, the RawParser may come into situations where
	/// multiple events need to be pushed from a single token, which is why
	/// the queue exists as a buffer.
	eventq: VecDeque<RawEvent>,
	err: Option<Box<Error>>,
	comments: CommentMode,
}

impl InnerParser {
	fn new(comments: CommentMode) -> Self {
		Self {
			state: State::Initial,
			element_stack: Vec::new(),
			attribute_scratchpad: None,
			event_last_token_end: None,
			event_length: 0,
			pre_event_length: 0,
			eventq: VecDeque::new(),
			err: None,
			comments,
		}
	}

	fn account_invisible_token(&mut self, tm: &TokenMetrics) -> Result<()> {
		if self.event_last_token_end.is_some() {
			self.account_token(tm)?;
		} else {
			self.pre_event_length = self
				.pre_event_length
				.checked_add(tm.len())
				.ok_or(Error::RestrictedXml("event too long"))?;
		}
		Ok(())
	}

	fn start_event(&mut self, tm: &TokenMetrics) {
		debug_assert!(self.event_last_token_end.is_none());
		self.event_last_token_end = Some(tm.end());
		self.event_length = tm.len() + self.pre_event_length;
		self.pre_event_length = 0;
	}

	fn account_token(&mut self, tm: &TokenMetrics) -> Result<usize> {
		let last_end = self.event_last_token_end.unwrap();
		self.event_length = self
			.event_length
			.checked_add(tm.len() + tm.start().saturating_sub(last_end))
			.ok_or(Error::RestrictedXml("event too long"))?;
		self.event_last_token_end = Some(tm.end());
		Ok(self.event_length)
	}

	fn finish_event(&mut self) -> EventMetrics {
		debug_assert!(self.event_last_token_end.is_some());
		let len = self.event_length;
		self.event_last_token_end = None;
		self.event_length = 0;
		EventMetrics { len }
	}

	fn fixed_event(&self, len: usize) -> EventMetrics {
		debug_assert!(self.event_last_token_end.is_none());
		EventMetrics { len }
	}

	fn read_token<T: TokenRead>(&mut self, r: &mut T) -> Result<Option<Token>> {
		if self.event_last_token_end.is_none() {
			return r.read();
		}
		match r.read()? {
			Some(tok) => {
				self.account_token(tok.metrics())?;
				Ok(Some(tok))
			}
			None => Ok(None),
		}
	}

	/// Emit an event into the event queue.
	fn emit_event(&mut self, ev: RawEvent) {
		self.eventq.push_back(ev);
	}

	/// Poison the parser, making it return the same error for all eternity.
	fn poison(&mut self, e: Error) {
		self.err = Some(Box::new(e))
	}

	/// Check if the parser is poisoned and return the corresponding error.
	fn check_poison(&self) -> Result<()> {
		if let Some(e) = self.err.as_ref() {
			Err((**e).into())
		} else {
			Ok(())
		}
	}

	/// Initialize the element scratchpad for further processing.
	///
	/// May fail if the name is not namespace-well-formed.
	fn start_processing_element(&mut self, name: Name) -> Result<RawEvent> {
		self.element_stack.push(name.clone());
		let (prefix, localname) = name
			.split_name()
			.map_err(|e| Error::from_validation(e, Some(ErrorContext::Element)))?;
		Ok(RawEvent::ElementHeadOpen(
			self.finish_event(),
			(prefix, localname),
		))
	}

	/// Pop an element off the stack and emit the corresponding EndElement
	/// event.
	fn pop_element(&mut self, em: EventMetrics) -> Result<State> {
		let ev = RawEvent::ElementFoot(em);
		self.emit_event(ev);
		debug_assert!(!self.element_stack.is_empty());
		self.element_stack.pop();
		if self.element_stack.is_empty() {
			Ok(State::End)
		} else {
			Ok(State::Document(DocSt::CData))
		}
	}

	fn handle_comment_start(
		&mut self,
		metrics: &TokenMetrics,
		ret: CommentReturn,
	) -> Result<State> {
		match self.comments {
			CommentMode::Reject => Err(EndOrError::Error(Error::RestrictedXml("comments"))),
			CommentMode::Discard => {
				self.account_invisible_token(metrics)?;
				Ok(State::Comment(ret))
			}
		}
	}

	/// Initial parser state.
	///
	/// See [`State::Initial`].
	fn parse_initial<T: TokenRead>(&mut self, r: &mut T) -> Result<State> {
		match self.read_token(r)? {
			Some(Token::XMLDeclStart(tm)) => {
				self.start_event(&tm);
				Ok(State::Decl {
					substate: DeclSt::VersionName,
					version: None,
				})
			}
			Some(Token::ElementHeadStart(tm, name)) => {
				self.start_event(&tm);
				let ev = self.start_processing_element(name)?;
				self.emit_event(ev);
				// We have to start the event for the attribute name or for
				// the closing symbol here, in order to account for whitespace
				// between the things.
				self.start_event(&tm);
				self.event_length = 0;
				Ok(State::Document(DocSt::Element(ElementSt::AttrName)))
			}
			Some(Token::CommentStart(tm)) => self.handle_comment_start(&tm, CommentReturn::Element),
			Some(tok) => Err(EndOrError::Error(Error::UnexpectedToken(
				Some(ErrorContext::DocumentBegin),
				tok.name(),
				Some(&[Token::NAME_ELEMENTHEADSTART, Token::NAME_XMLDECLSTART]),
			))),
			None => Err(EndOrError::Error(Error::wfeof(ErrorContext::DocumentBegin))),
		}
	}

	/// XML declaration state.
	///
	/// See [`State::Decl`].
	fn parse_decl<T: TokenRead>(
		&mut self,
		state: DeclSt,
		version: Option<XmlVersion>,
		r: &mut T,
	) -> Result<State> {
		match self.read_token(r)? {
			None => Err(EndOrError::Error(Error::wfeof(
				ErrorContext::XmlDeclaration,
			))),
			Some(Token::Name(_, name)) => {
				match state {
					DeclSt::VersionName => {
						if name == "version" {
							Ok(State::Decl {
								substate: DeclSt::VersionEq,
								version,
							})
						} else {
							Err(EndOrError::Error(Error::InvalidSyntax(
								"'<?xml' must be followed by version attribute",
							)))
						}
					}
					DeclSt::EncodingName => {
						if name == "encoding" {
							Ok(State::Decl {
								substate: DeclSt::EncodingEq,
								version,
							})
						} else {
							Err(EndOrError::Error(Error::InvalidSyntax(
								"'version' attribute must be followed by '?>' or 'encoding' attribute",
							)))
						}
					}
					DeclSt::StandaloneName => {
						if name == "standalone" {
							Ok(State::Decl {
								substate: DeclSt::StandaloneEq,
								version,
							})
						} else {
							Err(EndOrError::Error(Error::InvalidSyntax(
								"'encoding' attribute must be followed by '?>' or 'standalone' attribute",
							)))
						}
					}
					_ => Err(EndOrError::Error(Error::UnexpectedToken(
						Some(ErrorContext::XmlDeclaration),
						Token::NAME_NAME,
						None, // TODO: add expected tokens here
					))),
				}
			}
			Some(Token::Eq(_)) => Ok(State::Decl {
				substate: match state {
					DeclSt::VersionEq => Ok(DeclSt::VersionValue),
					DeclSt::EncodingEq => Ok(DeclSt::EncodingValue),
					DeclSt::StandaloneEq => Ok(DeclSt::StandaloneValue),
					_ => Err(Error::UnexpectedToken(
						Some(ErrorContext::XmlDeclaration),
						Token::NAME_EQ,
						None,
					)),
				}?,
				version,
			}),
			Some(Token::AttributeValue(_, v)) => match state {
				DeclSt::VersionValue => {
					if v == "1.0" {
						Ok(State::Decl {
							substate: DeclSt::EncodingName,
							version: Some(XmlVersion::V1_0),
						})
					} else {
						Err(EndOrError::Error(Error::RestrictedXml(
							"only XML version 1.0 is allowed",
						)))
					}
				}
				DeclSt::EncodingValue => {
					if v.eq_ignore_ascii_case("utf-8") {
						Ok(State::Decl {
							substate: DeclSt::StandaloneName,
							version,
						})
					} else {
						Err(EndOrError::Error(Error::RestrictedXml(
							"only utf-8 encoding is allowed",
						)))
					}
				}
				DeclSt::StandaloneValue => {
					if v.eq_ignore_ascii_case("yes") {
						Ok(State::Decl {
							substate: DeclSt::Close,
							version,
						})
					} else {
						Err(EndOrError::Error(Error::RestrictedXml(
							"only standalone documents are allowed",
						)))
					}
				}
				_ => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::XmlDeclaration),
					Token::NAME_ATTRIBUTEVALUE,
					None,
				))),
			},
			Some(Token::XMLDeclEnd(_)) => match state {
				DeclSt::EncodingName | DeclSt::StandaloneName | DeclSt::Close => {
					let ev = RawEvent::XmlDeclaration(self.finish_event(), version.unwrap());
					self.emit_event(ev);
					Ok(State::Document(DocSt::Element(ElementSt::Expected)))
				}
				_ => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::XmlDeclaration),
					Token::NAME_XMLDECLEND,
					None,
				))),
			},
			Some(other) => Err(EndOrError::Error(Error::UnexpectedToken(
				Some(ErrorContext::XmlDeclaration),
				other.name(),
				None,
			))),
		}
	}

	/// Finalize a single attribute and return the event.
	fn finalize_attribute(&mut self, val: String) -> Result<RawEvent> {
		let (prefix, localpart) = self.attribute_scratchpad.take().unwrap();
		if let Some(prefix) = prefix.as_ref() {
			if prefix == "xmlns" {
				// Namespaces for XML 1.0
				// Namespace constraint: Reserved Prefixes and Namespace Names
				if localpart == "xml" {
					if val != XMLNS_XML {
						return Err(EndOrError::Error(Error::ReservedNamespacePrefix));
					}
				} else {
					if val == XMLNS_XML {
						return Err(EndOrError::Error(Error::ReservedNamespaceName));
					}
				}
				// Namespaces for XML 1.0
				// Namespace constraint: No Prefix Undeclaring
				if val.is_empty() {
					return Err(EndOrError::Error(Error::EmptyNamespaceUri));
				}
			}
		} else if localpart == "xmlns" && val == XMLNS_XML {
			// Namespaces for XML 1.0
			// Namespace constraint: Reserved Prefixes and Namespace Names
			return Err(EndOrError::Error(Error::ReservedNamespaceName));
		}
		Ok(RawEvent::Attribute(
			self.finish_event(),
			(prefix, localpart),
			val,
		))
	}

	/// Element state
	///
	/// See [`DocSt::Element`].
	fn parse_element<T: TokenRead>(&mut self, state: ElementSt, r: &mut T) -> Result<State> {
		match self.read_token(r)? {
			None => match state {
				ElementSt::Expected => {
					Err(EndOrError::Error(Error::wfeof(ErrorContext::DocumentBegin)))
				}
				_ => Err(EndOrError::Error(Error::wfeof(ErrorContext::Element))),
			},
			// this could be <?xml-stylesheet or some other processing
			// so we reject it here appropriately.
			Some(Token::XMLDeclStart(..)) if state == ElementSt::Expected => Err(
				EndOrError::Error(Error::RestrictedXml("processing instructions")),
			),
			Some(Token::ElementHeadStart(tm, name)) if state == ElementSt::Expected => {
				self.start_event(&tm);
				let ev = self.start_processing_element(name)?;
				self.emit_event(ev);
				// We have to start the event for the attribute name or for
				// the closing symbol here, in order to account for whitespace
				// between the things.
				self.start_event(&tm);
				self.event_length = 0;
				Ok(State::Document(DocSt::Element(ElementSt::AttrName)))
			}
			Some(Token::CommentStart(tm)) if state == ElementSt::Expected => {
				self.handle_comment_start(&tm, CommentReturn::Element)
			}
			Some(Token::ElementHFEnd(_)) => match state {
				ElementSt::AttrName => {
					// the event must have been started by the previous
					// Token::AttrValue or by the Token::ElementHeadStart
					assert!(self.event_last_token_end.is_some());
					let em = self.finish_event();
					self.emit_event(RawEvent::ElementHeadClose(em));
					Ok(State::Document(DocSt::CData))
				}
				_ => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::Element),
					Token::NAME_ELEMENTHEADCLOSE,
					None,
				))),
			},
			Some(Token::ElementHeadClose(_)) => match state {
				ElementSt::AttrName => {
					// the event must have been started by the previous
					// Token::AttrValue or by the Token::ElementHeadStart
					assert!(self.event_last_token_end.is_some());
					let em = self.finish_event();
					self.emit_event(RawEvent::ElementHeadClose(em));
					Ok(self.pop_element(self.fixed_event(0))?)
				}
				_ => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::Element),
					Token::NAME_ELEMENTHEADCLOSE,
					None,
				))),
			},
			Some(Token::Name(_, name)) => match state {
				ElementSt::AttrName => {
					// the event must have been started by the previous
					// Token::AttrValue or by the Token::ElementHeadStart
					assert!(self.event_last_token_end.is_some());
					let (prefix, localname) = name.split_name().map_err(|e| {
						Error::from_validation(e, Some(ErrorContext::AttributeName))
					})?;
					if let Some(prefix) = prefix.as_ref() {
						if prefix == "xmlns" && localname == "xmlns" {
							return Err(EndOrError::Error(Error::ReservedNamespacePrefix));
						}
					}
					self.attribute_scratchpad = Some((prefix, localname));
					Ok(State::Document(DocSt::Element(ElementSt::AttrEq)))
				}
				_ => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::Element),
					Token::NAME_NAME,
					None,
				))),
			},
			Some(Token::Eq(_)) => match state {
				ElementSt::AttrEq => Ok(State::Document(DocSt::Element(ElementSt::AttrValue))),
				_ => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::Element),
					Token::NAME_EQ,
					None,
				))),
			},
			Some(Token::AttributeValue(tm, val)) => match state {
				ElementSt::AttrValue => {
					let ev = self.finalize_attribute(val)?;
					self.emit_event(ev);
					// We have to start the event for further attribute names
					// or for the closing symbol here, in order to account for
					// whitespace between the things.
					self.start_event(&tm);
					self.event_length = 0;
					Ok(State::Document(DocSt::Element(ElementSt::AttrName)))
				}
				_ => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::Element),
					Token::NAME_EQ,
					None,
				))),
			},
			Some(tok) => Err(EndOrError::Error(Error::UnexpectedToken(
				Some(ErrorContext::Element),
				tok.name(),
				None,
			))),
		}
	}

	/// Document content state
	///
	/// See [`State::Document`].
	fn parse_document<T: TokenRead>(&mut self, state: DocSt, r: &mut T) -> Result<State> {
		match state {
			DocSt::Element(substate) => self.parse_element(substate, r),
			DocSt::CData => match self.read_token(r)? {
				Some(Token::Text(tm, s)) => {
					self.start_event(&tm);
					let ev = RawEvent::Text(self.finish_event(), s);
					self.emit_event(ev);
					Ok(State::Document(DocSt::CData))
				}
				Some(Token::ElementHeadStart(tm, name)) => {
					self.start_event(&tm);
					let ev = self.start_processing_element(name)?;
					self.emit_event(ev);
					// We have to start the event for the attribute name or for
					// the closing symbol here, in order to account for
					// whitespace between the things.
					self.start_event(&tm);
					self.event_length = 0;
					Ok(State::Document(DocSt::Element(ElementSt::AttrName)))
				}
				Some(Token::ElementFootStart(tm, name)) => {
					self.start_event(&tm);
					if self.element_stack[self.element_stack.len() - 1] != name {
						Err(EndOrError::Error(Error::ElementMismatch))
					} else {
						Ok(State::Document(DocSt::ElementFoot))
					}
				}
				// this could be <?xml-stylesheet or some other processing
				// so we reject it here appropriately.
				Some(Token::XMLDeclStart(..)) => Err(EndOrError::Error(Error::RestrictedXml(
					"processing instructions",
				))),
				Some(Token::CommentStart(tm)) => {
					self.handle_comment_start(&tm, CommentReturn::CData)
				}
				Some(tok) => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::Text),
					tok.name(),
					Some(&[
						Token::NAME_TEXT,
						Token::NAME_ELEMENTHEADSTART,
						Token::NAME_ELEMENTFOOTSTART,
						Token::NAME_COMMENTSTART,
					]),
				))),
				None => Err(EndOrError::Error(Error::wfeof(ErrorContext::Text))),
			},
			DocSt::ElementFoot => match self.read_token(r)? {
				Some(Token::ElementHFEnd(_)) => {
					let ev = self.finish_event();
					self.pop_element(ev)
				}
				Some(other) => Err(EndOrError::Error(Error::UnexpectedToken(
					Some(ErrorContext::ElementFoot),
					other.name(),
					Some(&[Token::NAME_ELEMENTHFEND]),
				))),
				None => Err(EndOrError::Error(Error::wfeof(ErrorContext::ElementFoot))),
			},
		}
	}

	fn parse_comment<T: TokenRead>(&mut self, ret: CommentReturn, r: &mut T) -> Result<State> {
		match self.read_token(r)? {
			Some(Token::Text(tm, _)) => {
				self.account_invisible_token(&tm)?;
				Ok(State::Comment(ret))
			}
			Some(Token::CommentEnd(tm)) => {
				self.account_invisible_token(&tm)?;
				Ok(ret.into())
			}
			Some(tok) => Err(EndOrError::Error(Error::UnexpectedToken(
				Some(ErrorContext::Comment),
				tok.name(),
				Some(&[Token::NAME_TEXT, Token::NAME_COMMENTEND]),
			))),
			None => Err(EndOrError::Error(Error::wfeof(ErrorContext::Comment))),
		}
	}

	fn parse<T: TokenRead>(&mut self, r: &mut T) -> Result<Option<RawEvent>> {
		self.check_poison()?;
		loop {
			if !self.eventq.is_empty() {
				return Ok(Some(self.eventq.pop_front().unwrap()));
			}

			let result = match self.state {
				State::Initial => self.parse_initial(r),
				State::Decl { substate, version } => self.parse_decl(substate, version, r),
				State::Document(substate) => self.parse_document(substate, r),
				State::Comment(ret) => self.parse_comment(ret, r),
				State::End => match self.read_token(r)? {
					None => Ok(State::Eof),
					// whitespace after the root element is explicitly allowed
					Some(Token::Text(_, s))
						if s.as_bytes()
							.iter()
							.all(|&c| c == b' ' || c == b'\t' || c == b'\n' || c == b'\r') =>
					{
						Ok(State::End)
					}
					Some(tok) => Err(EndOrError::Error(Error::UnexpectedToken(
						Some(ErrorContext::DocumentEnd),
						tok.name(),
						Some(&["end-of-file"]),
					))),
				},
				State::Eof => return Ok(None),
			};
			self.state = match result {
				Ok(st) => st,
				// pass lack of data through without poisoning the parser
				Err(EndOrError::NeedMoreData) => return Err(EndOrError::NeedMoreData),
				// poison the parser for everything else to avoid emitting illegal data
				Err(EndOrError::Error(other)) => {
					self.poison(other);
					return Err(EndOrError::Error(other));
				}
			};
		}
	}

	fn release_temporaries(&mut self) {
		self.eventq.shrink_to_fit();
		self.element_stack.shrink_to_fit();
	}
}

impl Parse for RawParser {
	type Output = RawEvent;

	fn parse(&mut self, r: &'_ mut &'_ [u8], at_eof: bool) -> Result<Option<Self::Output>> {
		self.inner.parse(&mut ByteTokenReader {
			lexer: &mut self.lexer,
			buf: r,
			at_eof,
		})
	}

	fn release_temporaries(&mut self) {
		self.inner.release_temporaries();
		self.lexer.release_temporaries();
	}
}

impl fmt::Debug for InnerParser {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("InnerParser")
			.field("state", &self.state)
			.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lexer::TokenMetrics;
	use std::println;

	const TEST_NS: &'static str = "urn:uuid:4e1c8b65-ae37-49f8-a250-c27d52827da9";

	const DM: TokenMetrics = TokenMetrics::new(0, 0);

	// XXX: this should be possible without a subtype *shrug*
	struct TokenSliceReader<'x> {
		base: &'x [Token],
		offset: usize,
	}

	struct TokenResultReader<I> {
		base: I,
	}

	impl<I: Iterator<Item = Result<Token>>> TokenResultReader<I> {
		fn new<T: IntoIterator<IntoIter = I>>(iter: T) -> Self {
			Self {
				base: iter.into_iter(),
			}
		}
	}

	struct SometimesBlockingTokenSliceReader<'x> {
		base: &'x [Token],
		offset: usize,
		has_blocked: bool,
	}

	trait TokenSliceWrapper<'x> {
		fn new(src: &'x [Token]) -> Self;
	}

	impl<'x> TokenSliceWrapper<'x> for TokenSliceReader<'x> {
		fn new(src: &'x [Token]) -> TokenSliceReader<'x> {
			TokenSliceReader {
				base: src,
				offset: 0,
			}
		}
	}

	impl<'x> TokenSliceWrapper<'x> for SometimesBlockingTokenSliceReader<'x> {
		fn new(src: &'x [Token]) -> SometimesBlockingTokenSliceReader<'x> {
			SometimesBlockingTokenSliceReader {
				base: src,
				offset: 0,
				has_blocked: false,
			}
		}
	}

	impl<'x> TokenRead for TokenSliceReader<'x> {
		fn read(&mut self) -> Result<Option<Token>> {
			match self.base.get(self.offset) {
				Some(x) => {
					self.offset += 1;
					let result = x.clone();
					println!("returning token {:?}", result);
					Ok(Some(result))
				}
				None => Ok(None),
			}
		}
	}

	impl<I: Iterator<Item = Result<Token>>> TokenRead for TokenResultReader<I> {
		fn read(&mut self) -> Result<Option<Token>> {
			self.base.next().transpose()
		}
	}

	impl<'x> TokenRead for SometimesBlockingTokenSliceReader<'x> {
		fn read(&mut self) -> Result<Option<Token>> {
			if !self.has_blocked {
				self.has_blocked = true;
				return Err(EndOrError::NeedMoreData);
			}

			match self.base.get(self.offset) {
				Some(x) => {
					self.has_blocked = false;
					self.offset += 1;
					let result = x.clone();
					println!("returning token {:?}", result);
					Ok(Some(result))
				}
				None => Ok(None),
			}
		}
	}

	fn parse_custom<'t, T: TokenSliceWrapper<'t> + TokenRead>(
		src: &'t [Token],
		comments: CommentMode,
	) -> (Vec<RawEvent>, Result<()>) {
		let mut sink = Vec::new();
		let mut reader = T::new(src);
		let mut parser = InnerParser::new(comments);
		loop {
			match parser.parse(&mut reader) {
				Ok(Some(ev)) => sink.push(ev),
				Ok(None) => return (sink, Ok(())),
				Err(e) => return (sink, Err(e)),
			}
		}
	}

	fn parse(src: &[Token]) -> (Vec<RawEvent>, Result<()>) {
		let (a_no_comments, b_no_comments) =
			parse_custom::<TokenSliceReader>(src, CommentMode::Reject);
		let (a_comments, b_comments) = parse_custom::<TokenSliceReader>(src, CommentMode::Discard);
		assert_eq!(a_comments, a_no_comments);
		assert_eq!(b_comments, b_no_comments);
		(a_no_comments, b_no_comments)
	}

	fn parse_with_comments(src: &[Token]) -> (Vec<RawEvent>, Result<()>) {
		parse_custom::<TokenSliceReader>(src, CommentMode::Discard)
	}

	fn parse_without_comments(src: &[Token]) -> (Vec<RawEvent>, Result<()>) {
		parse_custom::<TokenSliceReader>(src, CommentMode::Reject)
	}

	fn parse_err(src: &[Token]) -> Option<EndOrError> {
		let (_, r) = parse(src);
		r.err()
	}

	#[test]
	fn parser_parse_xml_declaration() {
		let (evs, r) = parse(&[
			Token::XMLDeclStart(TokenMetrics::new(0, 1)),
			Token::Name(TokenMetrics::new(2, 3), "version".try_into().unwrap()),
			Token::Eq(TokenMetrics::new(3, 4)),
			Token::AttributeValue(TokenMetrics::new(4, 5), "1.0".try_into().unwrap()),
			Token::XMLDeclEnd(TokenMetrics::new(6, 7)),
		]);
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::XmlDeclaration(em, XmlVersion::V1_0) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		assert!(iter.next().is_none());
		match r {
			Err(EndOrError::Error(Error::InvalidEof(Some(ErrorContext::DocumentBegin)))) => (),
			other => panic!("unexpected result: {other:?}"),
		}
	}

	#[test]
	fn parser_rejects_comments_before_xml_declaration_without_comments() {
		let (evs, r) = parse_without_comments(&[
			Token::CommentStart(TokenMetrics::new(0, 1)),
			Token::CommentEnd(TokenMetrics::new(1, 2)),
			Token::XMLDeclStart(TokenMetrics::new(2, 3)),
			Token::Name(TokenMetrics::new(3, 4), "version".try_into().unwrap()),
			Token::Eq(TokenMetrics::new(4, 5)),
			Token::AttributeValue(TokenMetrics::new(5, 6), "1.0".try_into().unwrap()),
			Token::XMLDeclEnd(TokenMetrics::new(6, 7)),
		]);
		let mut iter = evs.iter();
		assert!(iter.next().is_none());
		match r {
			Err(EndOrError::Error(Error::RestrictedXml("comments"))) => (),
			other => panic!("unexpected result: {other:?}"),
		}
	}

	#[test]
	fn parser_rejects_comments_before_xml_declaration_with_comments() {
		let (evs, r) = parse_with_comments(&[
			Token::CommentStart(TokenMetrics::new(0, 1)),
			Token::CommentEnd(TokenMetrics::new(1, 2)),
			Token::XMLDeclStart(TokenMetrics::new(2, 3)),
			Token::Name(TokenMetrics::new(3, 4), "version".try_into().unwrap()),
			Token::Eq(TokenMetrics::new(4, 5)),
			Token::AttributeValue(TokenMetrics::new(5, 6), "1.0".try_into().unwrap()),
			Token::XMLDeclEnd(TokenMetrics::new(6, 7)),
		]);
		let mut iter = evs.iter();
		assert!(iter.next().is_none());
		match r {
			// the xml declaration looks like PI to the parser
			Err(EndOrError::Error(Error::RestrictedXml("processing instructions"))) => (),
			other => panic!("unexpected result: {other:?}"),
		}
	}

	#[test]
	fn parser_discards_comments_after_xml_declaration() {
		let (evs, r) = parse_with_comments(&[
			Token::XMLDeclStart(TokenMetrics::new(0, 1)),
			Token::Name(TokenMetrics::new(1, 2), "version".try_into().unwrap()),
			Token::Eq(TokenMetrics::new(3, 4)),
			Token::AttributeValue(TokenMetrics::new(5, 6), "1.0".try_into().unwrap()),
			Token::XMLDeclEnd(TokenMetrics::new(6, 7)),
			Token::CommentStart(TokenMetrics::new(7, 8)),
			Token::CommentEnd(TokenMetrics::new(8, 9)),
		]);
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::XmlDeclaration(em, XmlVersion::V1_0) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		assert!(iter.next().is_none());
		match r {
			Err(EndOrError::Error(Error::InvalidEof(Some(ErrorContext::DocumentBegin)))) => (),
			other => panic!("unexpected result: {other:?}"),
		}
	}

	#[test]
	fn parser_discards_comments_after_implicit_xml_declaration() {
		let (evs, r) = parse_with_comments(&[
			Token::CommentStart(TokenMetrics::new(0, 1)),
			Token::CommentEnd(TokenMetrics::new(1, 2)),
		]);
		let mut iter = evs.iter();
		assert!(iter.next().is_none());
		match r {
			Err(EndOrError::Error(Error::InvalidEof(Some(ErrorContext::DocumentBegin)))) => (),
			other => panic!("unexpected result: {other:?}"),
		}
	}

	#[test]
	fn parser_parse_wouldblock_as_first_token() {
		struct DegenerateTokenSource();

		impl TokenRead for DegenerateTokenSource {
			fn read(&mut self) -> Result<Option<Token>> {
				Err(EndOrError::NeedMoreData)
			}
		}

		let mut reader = DegenerateTokenSource();
		let mut parser = InnerParser::new(CommentMode::Reject);
		let r = parser.parse(&mut reader);
		assert!(matches!(r.err().unwrap(), EndOrError::NeedMoreData));
	}

	#[test]
	fn parser_recovers_from_wouldblock() {
		let toks = &[
			Token::XMLDeclStart(DM),
			Token::Name(DM, "version".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "1.0".try_into().unwrap()),
			Token::XMLDeclEnd(DM),
		];
		let mut reader = SometimesBlockingTokenSliceReader::new(toks);
		let mut parser = InnerParser::new(CommentMode::Reject);
		let mut evs = Vec::new();

		loop {
			match parser.parse(&mut reader) {
				Err(EndOrError::NeedMoreData) => continue,
				Err(EndOrError::Error(Error::InvalidEof(Some(ErrorContext::DocumentBegin)))) => {
					break;
				}
				Err(other) => panic!("unexpected error: {:?}", other),
				Ok(Some(ev)) => evs.push(ev),
				Ok(None) => panic!("unexpected eof: {:?}", parser),
			}
		}
		assert!(matches!(
			&evs[0],
			RawEvent::XmlDeclaration(EventMetrics { len: 0 }, XmlVersion::V1_0)
		));
		assert_eq!(evs.len(), 1);
	}

	#[test]
	fn parser_parse_stepwise() {
		let toks = &[
			Token::XMLDeclStart(DM),
			Token::Name(DM, "version".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "1.0".try_into().unwrap()),
			Token::XMLDeclEnd(DM),
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
		];
		let mut reader = TokenSliceReader::new(toks);
		let mut parser = InnerParser::new(CommentMode::Reject);
		let r = parser.parse(&mut reader);
		assert!(matches!(
			r.unwrap().unwrap(),
			RawEvent::XmlDeclaration(EventMetrics { len: 0 }, XmlVersion::V1_0)
		));
	}

	#[test]
	fn parser_parse_element_after_xml_declaration() {
		let (mut evs, r) = parse(&[
			Token::XMLDeclStart(DM),
			Token::Name(DM, "version".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "1.0".try_into().unwrap()),
			Token::XMLDeclEnd(DM),
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		r.unwrap();
		match evs.remove(0) {
			RawEvent::XmlDeclaration(_, XmlVersion::V1_0) => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match evs.remove(0) {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match evs.iter().next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_element_without_decl() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		r.unwrap();
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_element_with_attr() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "foo".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "bar".try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		r.unwrap();
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "foo");
				assert_eq!(value, "bar");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
	}

	#[test]
	fn parser_parse_element_with_xmlns() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, TEST_NS.try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		r.unwrap();
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "xmlns");
				assert_eq!(value, TEST_NS);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
	}

	#[test]
	fn parser_parse_attribute_without_namespace_prefix() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, TEST_NS.try_into().unwrap()),
			Token::Name(DM, "foo".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "bar".try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		r.unwrap();
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "xmlns");
				assert_eq!(value, TEST_NS);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "foo");
				assert_eq!(value, "bar");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
	}

	#[test]
	fn parser_parse_attribute_with_namespace_prefix() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns:foo".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, TEST_NS.try_into().unwrap()),
			Token::Name(DM, "foo:bar".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "baz".try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		r.unwrap();
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.unwrap(), "xmlns");
				assert_eq!(localname, "foo");
				assert_eq!(value, TEST_NS);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.unwrap(), "foo");
				assert_eq!(localname, "bar");
				assert_eq!(value, "baz");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
	}

	#[test]
	fn parser_parse_reject_reserved_xmlns_prefix() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns:xmlns".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "baz".try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match r {
			Err(EndOrError::Error(Error::ReservedNamespacePrefix)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		assert_eq!(evs.len(), 0);
	}

	#[test]
	fn parser_parse_allow_xml_redeclaration() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns:xml".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(
				DM,
				"http://www.w3.org/XML/1998/namespace".try_into().unwrap(),
			),
			Token::ElementHeadClose(DM),
		]);
		r.unwrap();
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.unwrap(), "xmlns");
				assert_eq!(localname, "xml");
				assert_eq!(value, "http://www.w3.org/XML/1998/namespace");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match evs.remove(0) {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
	}

	#[test]
	fn parser_parse_reject_reserved_xml_prefix_with_incorrect_value() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns:xml".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "baz".try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match r {
			Err(EndOrError::Error(Error::ReservedNamespacePrefix)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		assert_eq!(evs.len(), 0);
	}

	#[test]
	fn parser_parse_reject_binding_xml_namespace_name_to_other_prefix() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns:fnord".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, XMLNS_XML.try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match r {
			Err(EndOrError::Error(Error::ReservedNamespaceName)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		assert_eq!(evs.len(), 0);
	}

	#[test]
	fn parser_parse_reject_binding_xml_namespace_name_to_the_default_ns() {
		let (mut evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, XMLNS_XML.try_into().unwrap()),
			Token::ElementHeadClose(DM),
		]);
		match evs.remove(0) {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match r {
			Err(EndOrError::Error(Error::ReservedNamespaceName)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		assert_eq!(evs.len(), 0);
	}

	#[test]
	fn parser_parse_nested_elements() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementHeadStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		r.unwrap();
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "child");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
	}

	#[test]
	fn parser_parse_mixed_content() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "Hello".try_into().unwrap()),
			Token::ElementHeadStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "mixed".try_into().unwrap()),
			Token::ElementFootStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "world!".try_into().unwrap()),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		r.unwrap();
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 0);
				assert_eq!(v, "Hello");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "child");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 0);
				assert_eq!(v, "mixed");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 0);
				assert_eq!(v, "world!");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_mixed_content_with_comments() {
		let (evs, r) = parse_with_comments(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::CommentStart(DM),
			Token::Text(DM, "ignoreme".try_into().unwrap()),
			Token::CommentEnd(DM),
			Token::Text(DM, "Hello".try_into().unwrap()),
			Token::ElementHeadStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "mixed".try_into().unwrap()),
			Token::CommentStart(DM),
			Token::Text(DM, "ignoreme".try_into().unwrap()),
			Token::CommentEnd(DM),
			Token::ElementFootStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "world!".try_into().unwrap()),
			Token::CommentStart(DM),
			Token::Text(DM, "ignoreme".try_into().unwrap()),
			Token::CommentEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		r.unwrap();
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 0);
				assert_eq!(v, "Hello");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "child");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 0);
				assert_eq!(v, "mixed");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 0);
				assert_eq!(v, "world!");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_reject_mismested_elements() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementHeadStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "nonchild".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		match r {
			Err(EndOrError::Error(Error::ElementMismatch)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "child");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_prefixed_elements() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "x:root".try_into().unwrap()),
			Token::Name(DM, "foo".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "bar".try_into().unwrap()),
			Token::Name(DM, "xmlns:x".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, TEST_NS.try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementHeadStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "x:root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		r.unwrap();
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.as_ref().unwrap(), "x");
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "foo");
				assert_eq!(value, "bar");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.as_ref().unwrap(), "xmlns");
				assert_eq!(localname, "x");
				assert_eq!(value, TEST_NS);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "child");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_nested_prefixed_elements() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "x:root".try_into().unwrap()),
			Token::Name(DM, "foo".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "bar".try_into().unwrap()),
			Token::Name(DM, "xmlns:x".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, TEST_NS.try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementHeadStart(DM, "x:child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "x:child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "x:root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		r.unwrap();
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.as_ref().unwrap(), "x");
				assert_eq!(localname, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localname, "foo");
				assert_eq!(value, "bar");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Attribute(em, (prefix, localname), value) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.as_ref().unwrap(), "xmlns");
				assert_eq!(localname, "x");
				assert_eq!(value, TEST_NS);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localname)) => {
				assert_eq!(em.len(), 0);
				assert_eq!(prefix.as_ref().unwrap(), "x");
				assert_eq!(localname, "child");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_repeats_error_after_first_encounter() {
		let toks = &[
			Token::ElementHeadStart(DM, "x:root".try_into().unwrap()),
			Token::Name(DM, "xmlns:xml".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, TEST_NS.try_into().unwrap()),
			Token::Name(DM, "xmlns:y".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, TEST_NS.try_into().unwrap()),
			Token::Name(DM, "x:a".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "foo".try_into().unwrap()),
			Token::Name(DM, "y:a".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "foo".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "x:root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		];
		let mut reader = TokenSliceReader::new(toks);
		let mut parser = InnerParser::new(CommentMode::Reject);
		let r = parser.parse(&mut reader);
		match r {
			Ok(Some(RawEvent::ElementHeadOpen(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		let r = parser.parse(&mut reader);
		match r {
			Err(EndOrError::Error(Error::ReservedNamespacePrefix)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		let r = parser.parse(&mut reader);
		match r {
			Err(EndOrError::Error(Error::ReservedNamespacePrefix)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_repeats_error_in_comments() {
		let toks = &[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::CommentStart(DM),
			Token::Text(DM, "Hello".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::CommentEnd(DM),
			Token::ElementHeadStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "mixed".try_into().unwrap()),
			Token::ElementFootStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "world!".try_into().unwrap()),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		];
		let mut reader = TokenSliceReader::new(toks);
		let mut parser = InnerParser::new(CommentMode::Discard);
		match parser.parse(&mut reader) {
			Ok(Some(RawEvent::ElementHeadOpen(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		match parser.parse(&mut reader) {
			Ok(Some(RawEvent::ElementHeadClose(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		match parser.parse(&mut reader) {
			Err(EndOrError::Error(Error::UnexpectedToken(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		match parser.parse(&mut reader) {
			Err(EndOrError::Error(Error::UnexpectedToken(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn parser_parse_repeats_lexer_error_in_comments() {
		let toks = [
			Ok(Token::ElementHeadStart(DM, "root".try_into().unwrap())),
			Ok(Token::ElementHFEnd(DM)),
			Ok(Token::CommentStart(DM)),
			Ok(Token::Text(DM, "Hello".try_into().unwrap())),
			Err(EndOrError::Error(Error::RestrictedXml("foo"))),
		];
		let mut reader = TokenResultReader::new(toks);
		let mut parser = InnerParser::new(CommentMode::Discard);
		match parser.parse(&mut reader) {
			Ok(Some(RawEvent::ElementHeadOpen(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		match parser.parse(&mut reader) {
			Ok(Some(RawEvent::ElementHeadClose(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		match parser.parse(&mut reader) {
			Err(EndOrError::Error(Error::RestrictedXml("foo"))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
		match parser.parse(&mut reader) {
			Err(EndOrError::Error(Error::RestrictedXml("foo"))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn parser_rejects_empty_namespace_uri() {
		let toks = &[
			Token::ElementHeadStart(DM, "x:root".try_into().unwrap()),
			Token::Name(DM, "xmlns:x".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "x:root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		];
		let err = parse_err(toks).unwrap();
		match err {
			EndOrError::Error(Error::EmptyNamespaceUri) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn parser_allows_empty_namespace_uri_for_default_namespace() {
		let toks = &[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::Name(DM, "xmlns".try_into().unwrap()),
			Token::Eq(DM),
			Token::AttributeValue(DM, "".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		];
		let (_evs, r) = parse(toks);
		r.unwrap();
	}

	#[test]
	fn parser_reject_element_after_root_element() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementHeadStart(DM, "garbage".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "garbage".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(_) => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(_) => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match r {
			Err(EndOrError::Error(Error::UnexpectedToken(_, _, _))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn parser_reject_text_after_root_element() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "foo".try_into().unwrap()),
		]);
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(_) => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(_) => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match r {
			Err(EndOrError::Error(Error::UnexpectedToken(_, _, _))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn parser_allow_whitespace_after_root_element() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, " \t\r\n".try_into().unwrap()),
			Token::Text(DM, "\n\r\t ".try_into().unwrap()),
		]);
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(_) => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(_) => (),
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
		r.unwrap();
	}

	#[test]
	fn parser_does_not_panic_on_too_many_closing_elements() {
		let err = parse_err(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		match err {
			Some(EndOrError::Error(Error::UnexpectedToken(..))) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn parser_forwards_metrics() {
		let (evs, r) = parse(&[
			Token::ElementHeadStart(TokenMetrics::new(0, 2), "root".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(2, 3)),
			Token::Text(TokenMetrics::new(3, 8), "Hello".try_into().unwrap()),
			Token::ElementHeadStart(TokenMetrics::new(8, 11), "child".try_into().unwrap()),
			Token::Name(TokenMetrics::new(12, 13), "foo".try_into().unwrap()),
			Token::Eq(TokenMetrics::new(13, 15)),
			Token::AttributeValue(TokenMetrics::new(15, 18), "bar".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(18, 20)),
			Token::Text(TokenMetrics::new(20, 30), "mixed".try_into().unwrap()),
			Token::ElementFootStart(TokenMetrics::new(30, 31), "child".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(31, 33)),
			Token::Text(TokenMetrics::new(33, 40), "world!".try_into().unwrap()),
			Token::ElementFootStart(TokenMetrics::new(40, 42), "root".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(42, 45)),
		]);
		r.unwrap();
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, ..) => {
				assert_eq!(em.len(), 2);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em, ..) => {
				assert_eq!(em.len(), 1);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, ..) => {
				assert_eq!(em.len(), 5);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, ..) => {
				assert_eq!(em.len(), 3);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::Attribute(em, ..) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em, ..) => {
				assert_eq!(em.len(), 2);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, ..) => {
				assert_eq!(em.len(), 10);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em, ..) => {
				assert_eq!(em.len(), 3);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, ..) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em, ..) => {
				assert_eq!(em.len(), 5);
			}
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn parser_rejects_comments_without_comments() {
		let (evs, r) = parse_without_comments(&[
			Token::ElementHeadStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::CommentStart(DM),
			Token::Text(DM, "ignoreme".try_into().unwrap()),
			Token::CommentEnd(DM),
			Token::Text(DM, "Hello".try_into().unwrap()),
			Token::ElementHeadStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "mixed".try_into().unwrap()),
			Token::CommentStart(DM),
			Token::Text(DM, "ignoreme".try_into().unwrap()),
			Token::CommentEnd(DM),
			Token::ElementFootStart(DM, "child".try_into().unwrap()),
			Token::ElementHFEnd(DM),
			Token::Text(DM, "world!".try_into().unwrap()),
			Token::CommentStart(DM),
			Token::Text(DM, "ignoreme".try_into().unwrap()),
			Token::CommentEnd(DM),
			Token::ElementFootStart(DM, "root".try_into().unwrap()),
			Token::ElementHFEnd(DM),
		]);
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 0);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 0);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match r {
			Err(EndOrError::Error(Error::RestrictedXml("comments"))) => (),
			other => panic!("unexpected result: {other:?}"),
		}
		assert!(iter.next().is_none());
	}

	#[test]
	fn parser_attributes_comments_to_next_event() {
		let (evs, r) = parse_with_comments(&[
			Token::ElementHeadStart(TokenMetrics::new(0, 1), "root".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(1, 2)),
			Token::CommentStart(TokenMetrics::new(2, 3)),
			Token::Text(TokenMetrics::new(3, 4), "ignoreme".try_into().unwrap()),
			Token::CommentEnd(TokenMetrics::new(4, 5)),
			Token::Text(TokenMetrics::new(5, 6), "Hello".try_into().unwrap()),
			Token::ElementHeadStart(TokenMetrics::new(6, 7), "child".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(7, 8)),
			Token::Text(TokenMetrics::new(8, 9), "mixed".try_into().unwrap()),
			Token::CommentStart(TokenMetrics::new(9, 10)),
			Token::Text(TokenMetrics::new(10, 11), "ignoreme".try_into().unwrap()),
			Token::CommentEnd(TokenMetrics::new(11, 12)),
			Token::ElementFootStart(TokenMetrics::new(12, 13), "child".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(13, 14)),
			Token::Text(TokenMetrics::new(14, 15), "world!".try_into().unwrap()),
			Token::CommentStart(TokenMetrics::new(15, 16)),
			Token::Text(TokenMetrics::new(16, 17), "ignoreme".try_into().unwrap()),
			Token::CommentEnd(TokenMetrics::new(17, 18)),
			Token::ElementFootStart(TokenMetrics::new(18, 19), "root".try_into().unwrap()),
			Token::ElementHFEnd(TokenMetrics::new(19, 20)),
		]);
		r.unwrap();
		let mut iter = evs.iter();
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 1);
				assert!(prefix.is_none());
				assert_eq!(localpart, "root");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 1);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 4);
				assert_eq!(v, "Hello");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadOpen(em, (prefix, localpart)) => {
				assert_eq!(em.len(), 1);
				assert!(prefix.is_none());
				assert_eq!(localpart, "child");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementHeadClose(em) => {
				assert_eq!(em.len(), 1);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 1);
				assert_eq!(v, "mixed");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 5);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::Text(em, v) => {
				assert_eq!(em.len(), 1);
				assert_eq!(v, "world!");
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next().unwrap() {
			RawEvent::ElementFoot(em) => {
				assert_eq!(em.len(), 5);
			}
			ev => panic!("unexpected event: {:?}", ev),
		}
		match iter.next() {
			None => (),
			other => panic!("unexpected event: {:?}", other),
		}
	}
}
