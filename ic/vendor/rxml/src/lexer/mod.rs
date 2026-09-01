/*!
# XML 1.0 Lexer
*/
// needed for trait bounds
use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

mod ranges;
mod read;

use crate::error::{EndOrError, Error, ErrorContext, ErrorWithContext};
use crate::parser::Result;
use crate::strings::*;
use ranges::*;
use read::Endbyte;
use rxml_validation::selectors::*;

/// Carry information about where in the stream the token was observed
///
/// Tokens are not necessarily consecutive. Specifically, it is possible that
/// some whitespace is ignored and not converted into tokens between tokens
/// inside element headers and footers as well as between the XML declaration
/// and the first element.
#[derive(Copy, Debug, Clone, PartialEq, Eq)]
pub struct TokenMetrics {
	start: usize,
	end: usize,
}

impl TokenMetrics {
	/// Get the length of the token, taking a potential counter overflow
	/// into account.
	pub fn len(&self) -> usize {
		self.end.wrapping_sub(self.start)
	}

	/// Start byte in the stream.
	///
	/// Note that this is a "dumb" counter of size [`usize`] which may wrap
	/// around on some architectures with sufficently long-running streams.
	/// For accurate counting of bytes in a sequence of tokens, this needs
	/// to be taken into account.
	///
	/// Note also that more than one wraparound within a single token is
	/// generally not possible because the token length limit is also a
	/// `usize` and internal buffers will generally refuse to allocate before
	/// that limit is reached, even if set to usize::MAX.
	pub fn start(&self) -> usize {
		self.start
	}

	/// End byte of the token in the stream (exclusive).
	///
	/// Please see the considerations in [`TokenMetrics.start()`].
	pub fn end(&self) -> usize {
		self.end
	}

	// for use in parser unit tests
	#[cfg(test)]
	pub(crate) const fn new(start: usize, end: usize) -> TokenMetrics {
		TokenMetrics { start, end }
	}
}

/**
A single XML token

Tokens are emitted by the lexer after processing bits of XML. Tokens do not
map directly to concepts in the XML 1.0 specification. Instead, they are
modelled in such a way that they provide a useful layer of abstraction for
processing semantics inside the parser on top of the lexer.

Each token has a [`TokenMetrics`] object attached which describes the byte
range of the input stream from which the token was derived. Note that the
ranges denoted by the token metrics may not be consecutive, as some whitespace
within elements and the XML declaration does not generate tokens.
*/
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
	/// A freestanding (i.e. not the element name) XML `Name`.
	///
	/// This token is only emitted while the XML declaration or an element
	/// header or footer is being lexed.
	///
	/// See also [`Token::ElementHeadStart`] and [`Token::ElementFootStart`],
	/// which carry the XML element names.
	Name(TokenMetrics, Name),

	/// An equal sign.
	///
	/// This token is only emitted while the XML declaration or an element
	/// header or footer is being lexed.
	Eq(TokenMetrics),

	/// An attribute value.
	///
	/// The delimiters are not included in the CData. Any entity references
	/// are expanded already (i.e. you get `&` instead of `&amp;`).
	///
	/// Note that the number of bytes in an AttributeValue token will always
	/// be less than the number of bytes used to generate it. The delimiters
	/// are not included in its CData, but they are counted for the token
	/// metrics. Likewise, any entity references inside the attribute value
	/// will take more bytes "on the wire" than in the CData.
	///
	/// This token is only emitted while the XML declaration or an element
	/// header or footer is being lexed.
	AttributeValue(TokenMetrics, String),

	/// The `?>` sequence.
	///
	/// This token is only emitted while the XML declaration is being lexed.
	/// If the `?>` sequence is encountered in normal elements, an error is
	/// returned.
	XMLDeclEnd(TokenMetrics),

	/// The `/>` sequence.
	///
	/// This token is only emitted while an element header is being lexed. If
	/// a `/>` is encountered within an element footer or the XML declaration,
	/// an error is returned.
	ElementHeadClose(TokenMetrics),

	/// The `>` sequence.
	///
	/// This token is only emitted while an element header or footer is being
	/// lexed. If a stray `>` is encountered within the XML declaration, an
	/// error is returned.
	ElementHFEnd(TokenMetrics),

	/// The `<?xml` sequence.
	XMLDeclStart(TokenMetrics), // <?xml

	/// The `<` sequence, not followed by `/` or `?xml`.
	ElementHeadStart(TokenMetrics, Name),

	/// The `</` sequene.
	ElementFootStart(TokenMetrics, Name),

	/// A piece of character data inside an element.
	///
	/// Entity references and CDATA sections are processed in the lexer, which
	/// means that while `<![CDATA[foo]]>` will emit a text token with the
	/// contents `foo`, it is still possible to encounter the verbatim string
	/// `<![CDATA[foo]]>` inside a Text token (namely when the input
	/// `&lt;![CDATA[foo]]&gt` is processed).
	///
	/// There is no guarantee as to the segmentation of text tokens. It is
	/// possible that for a single consecutive piece of character data,
	/// multiple tokens are emitted. This can happen for instance when the
	/// token length limit is exceeded.
	///
	/// Note that the number of bytes in a Text token may be less than the
	/// number of bytes used to generate it. CDATA sections within the text
	/// as well as entity references generally boil down to fewer text UTF-8
	/// bytes than input bytes.
	///
	/// This token cannot occur while the XML declaration or an element header
	/// or footer is being lexed. Stray text eventually leads to an error
	/// there.
	Text(TokenMetrics, String),

	/// Start of a comment, i.e. `<!--`.
	///
	/// Comment contents are emitted as [`Text`][`Self::Text`] tokens. This is
	/// a by-product of technically having to validate the UTF-8 correctness
	/// of comment contents.
	CommentStart(TokenMetrics),

	/// End of a comment, i.e. `-->`.
	///
	/// This token is only emitted up to once after each `CommentStart` token.
	CommentEnd(TokenMetrics),
}

impl Token {
	pub const NAME_NAME: &'static str = "Name";
	pub const NAME_EQ: &'static str = "'='";
	pub const NAME_ATTRIBUTEVALUE: &'static str = "AttValue";
	pub const NAME_XMLDECLEND: &'static str = "'?>'";
	pub const NAME_ELEMENTHEADCLOSE: &'static str = "'/>'";
	pub const NAME_ELEMENTHFEND: &'static str = "'>'";
	pub const NAME_XMLDECLSTART: &'static str = "'<?xml'";
	pub const NAME_ELEMENTHEADSTART: &'static str = "'<'";
	pub const NAME_ELEMENTFOOTSTART: &'static str = "'</'";
	pub const NAME_TEXT: &'static str = "Text";
	pub const NAME_COMMENTSTART: &'static str = "'<!--'";
	pub const NAME_COMMENTEND: &'static str = "'-->'";

	/// Return a static string describing the token type.
	///
	/// This is intended for error messages.
	pub fn name(&self) -> &'static str {
		match self {
			Self::Name(..) => Self::NAME_NAME,
			Self::Eq(..) => Self::NAME_EQ,
			Self::AttributeValue(..) => Self::NAME_ATTRIBUTEVALUE,
			Self::XMLDeclEnd(..) => Self::NAME_XMLDECLEND,
			Self::ElementHeadClose(..) => Self::NAME_ELEMENTHEADCLOSE,
			Self::ElementHFEnd(..) => Self::NAME_ELEMENTHFEND,
			Self::XMLDeclStart(..) => Self::NAME_XMLDECLSTART,
			Self::ElementHeadStart(..) => Self::NAME_ELEMENTHEADSTART,
			Self::ElementFootStart(..) => Self::NAME_ELEMENTFOOTSTART,
			Self::Text(..) => Self::NAME_TEXT,
			Self::CommentStart(..) => Self::NAME_COMMENTSTART,
			Self::CommentEnd(..) => Self::NAME_COMMENTEND,
		}
	}

	/// Return a reference to this tokens [`TokenMetrics`].
	pub fn metrics(&self) -> &TokenMetrics {
		match self {
			Self::Name(m, ..) => m,
			Self::Eq(m) => m,
			Self::AttributeValue(m, ..) => m,
			Self::XMLDeclEnd(m) => m,
			Self::ElementHeadClose(m) => m,
			Self::ElementHFEnd(m) => m,
			Self::XMLDeclStart(m) => m,
			Self::ElementHeadStart(m, ..) => m,
			Self::ElementFootStart(m, ..) => m,
			Self::Text(m, ..) => m,
			Self::CommentStart(m, ..) => m,
			Self::CommentEnd(m, ..) => m,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CharRefRadix {
	Decimal,
	Hexadecimal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RefKind {
	Entity,
	Char(CharRefRadix),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ElementState {
	Start,
	/// Only used after <?xml
	SpaceRequired,
	Blank,
	Name,
	Eq,
	Close,
	/// Delimiter, Alphabet and whether we just read a CR, because of the mess
	/// which is CRLF -> LF normalization.
	AttributeValue(u8, bool),
	/// Encountered ?
	MaybeXMLDeclEnd,
	/// Encountered /
	MaybeHeadClose,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ElementKind {
	/// standard XML element head e.g. `<foo>`
	Header,
	/// standard XML element foot e.g. `</foo>`
	Footer,
	/// XML declaration e.g. `<?xml version='1.0'?>`
	XMLDecl,
}

/// State after `<!`
#[derive(Debug, Clone, Copy, PartialEq)]
enum Bang {
	Initial,
	/// Number of correct CDATA section start characters
	CDataSectionStart(usize),
	/// Number of correct comment start characters
	CommentStart(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MaybeElementState {
	Initial,
	/// Number of correct CDATA section start characters
	Bang(Bang),
	/// Number of correct XML decl start characters
	XMLDeclStart(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CrlfReturn {
	Normal,
	Cdata,
	Comment,
}

impl From<CrlfReturn> for ContentState {
	fn from(other: CrlfReturn) -> Self {
		match other {
			CrlfReturn::Normal => Self::Initial,
			CrlfReturn::Cdata => Self::CDataSection,
			CrlfReturn::Comment => Self::Comment,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ContentState {
	Initial,
	/// Within cdata section
	CDataSection,
	/// Encountered <
	MaybeElement(MaybeElementState),
	/// only whitespace allowed, e.g. between ?> and <
	Whitespace,
	/// `]]>` sequence, either within cdata (true) or not (false)
	/// if not within cdata, encountering this sequence is illegal
	MaybeCDataEnd(bool, usize),
	/// `\r` read, we need to look ahead by one char to see if it is a `\n`
	/// before substituting
	MaybeCRLF(CrlfReturn),
	/// Comment
	Comment,
	/// `-->` sequence.
	///
	/// Only entered from the `Comment` state.
	MaybeCommentEnd(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RefReturnState {
	AttributeValue(ElementKind, u8),
	Text,
}

impl RefReturnState {
	fn to_state(self) -> State {
		match self {
			Self::AttributeValue(kind, delim) => State::Element {
				kind,
				state: ElementState::AttributeValue(delim, false),
			},
			Self::Text => State::Content(ContentState::Initial),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
	Content(ContentState),
	Element {
		kind: ElementKind,
		state: ElementState,
	},

	/// encountered &
	Reference {
		ctx: ErrorContext,
		ret: RefReturnState,
		kind: RefKind,
	},

	Eof,
}

// longest text-based entity is 4 chars
// longest valid decimal entity is log(0x10ffff, 10) => 7
// longest valid hexadecimal entity is 6.
const MAX_REFERENCE_LENGTH: usize = 8usize;

const TOK_XML_DECL_START: &[u8] = b"<?xml";
const TOK_XML_CDATA_START: &[u8] = b"<![CDATA[";
const TOK_XML_COMMENT_START: &[u8] = b"<!--";
const TOK_XML_CDATA_END: &[u8] = b"]]>";
const TOK_XML_COMMENT_END: &[u8] = b"-->";
// const CLASS_XML_NAME_START_CHAR:

/// Hold options to configure a [`Lexer`].
///
/// See also [`Lexer::with_options()`].
#[derive(Debug, Clone, PartialEq, Copy)]
pub(crate) struct LexerOptions {
	/// Maximum number of bytes which can form a token.
	///
	/// This exists to limit the memory use of the Lexer for tokens where the
	/// data needs to be buffered in memory (most notably
	/// [`Token::Text`] and [`Token::AttributeValue`]).
	///
	/// If token data exceeds this limit, it depends on the token type whether
	/// a partial token is emitted or the lexing fails with
	/// [`Error::RestrictedXml`](crate::Error::RestrictedXml): Text tokens are
	/// split and emitted in parts (and lexing continues), all other tokens
	/// exceeding this limit will cause an error.
	pub max_token_length: usize,
}

fn resolve_named_entity(name: &[u8]) -> Result<u8> {
	// amp, lt, gt, apos, quot
	match name {
		b"amp" => Ok(b'&'),
		b"lt" => Ok(b'<'),
		b"gt" => Ok(b'>'),
		b"apos" => Ok(b'\''),
		b"quot" => Ok(b'"'),
		_ => Err(EndOrError::Error(Error::UndeclaredEntity)),
	}
}

fn resolve_char_reference(s: &str, radix: CharRefRadix, into: &mut Vec<u8>) -> Result<()> {
	let radix = match radix {
		CharRefRadix::Decimal => 10,
		CharRefRadix::Hexadecimal => 16,
	};
	// cannot fail because the string is validated against the alphabet and limited in length by the lexer
	let codepoint = u32::from_str_radix(s, radix).unwrap();
	let ch = match char::from_u32(codepoint) {
		Some(ch) => ch,
		None => return Err(EndOrError::Error(Error::InvalidChar(None, codepoint, true))),
	};
	if !CLASS_XML_NONCHAR.select(ch) {
		let mut buf = [0u8; 4];
		let s = ch.encode_utf8(&mut buf[..]);
		into.extend_from_slice(s.as_bytes());
		Ok(())
	} else {
		Err(EndOrError::Error(Error::InvalidChar(None, codepoint, true)))
	}
}

fn add_context<T>(r: Result<T>, ctx: ErrorContext) -> Result<T> {
	r.map_err(|e| e.with_context(ctx))
}

fn handle_eof<T>(v: Option<T>, ctx: ErrorContext) -> Result<T> {
	v.ok_or_else(|| EndOrError::Error(Error::wfeof(ctx)))
}

struct ST(State, Option<Token>);

impl ST {
	fn splice(self, st: &mut State) -> Option<Token> {
		*st = self.0;
		self.1
	}
}

/**
# Restricted XML 1.0 lexer

This lexer is able to lex a restricted subset of XML 1.0. For an overview
of the restrictions (including those imposed by [`Parser`](`crate::Parser`)),
see [`rxml`](`crate`).
*/
pub struct Lexer {
	state: State,
	scratchpad: Vec<u8>,
	swap: Vec<u8>,
	ctr: usize,
	last_token_end: usize,
	opts: LexerOptions,
	/// keep the scratchpad and state for debugging
	#[cfg(debug_assertions)]
	prev_state: (Vec<u8>, State),
	#[cfg(debug_assertions)]
	last_single_read: Option<u8>,
	err: Option<Error>,
	has_eof: bool,
	pub(crate) text_buffering: bool,
}

impl Lexer {
	/// Construct a new Lexer with the given options.
	pub fn new(opts: LexerOptions) -> Self {
		Self {
			state: State::Content(ContentState::Initial),
			scratchpad: Vec::new(),
			swap: Vec::new(),
			ctr: 0,
			last_token_end: 0,
			opts,
			#[cfg(debug_assertions)]
			prev_state: (Vec::new(), State::Content(ContentState::Initial)),
			#[cfg(debug_assertions)]
			last_single_read: None,
			err: None,
			has_eof: false,
			text_buffering: true,
		}
	}

	fn demote_eof(&self, ep: Endbyte) -> Result<Endbyte> {
		match ep {
			Endbyte::Eof => {
				if self.has_eof {
					Ok(Endbyte::Eof)
				} else {
					Err(EndOrError::NeedMoreData)
				}
			}
			other => Ok(other),
		}
	}

	fn token_length_error() -> EndOrError {
		EndOrError::Error(Error::RestrictedXml("long name or reference"))
	}

	fn eat_whitespace_metrics(&mut self, without: usize) {
		self.last_token_end = self.ctr.wrapping_sub(without);
	}

	#[inline]
	fn prep_scratchpad(&mut self) {
		if self.scratchpad.capacity() < self.opts.max_token_length {
			// unless there is a bug, we should never exceed the capacity requested by max_token_length, so we go with reserve_exact
			self.scratchpad
				.reserve_exact(self.opts.max_token_length - self.scratchpad.capacity())
		}
	}

	fn read_validated<B: ByteSelect + ?Sized>(
		&mut self,
		r: &mut &[u8],
		selector: &B,
		limit: usize,
	) -> Result<Endbyte> {
		let remaining = match limit.checked_sub(self.scratchpad.len()) {
			None => return Ok(Endbyte::Limit),
			Some(v) => v,
		};
		let old_len = self.scratchpad.len();
		self.prep_scratchpad();
		let ep = read::read_validated_bytes(r, selector, remaining, &mut self.scratchpad);
		self.ctr = self.ctr.wrapping_add(self.scratchpad.len() - old_len);
		if let Endbyte::Delimiter(_) = ep {
			self.ctr = self.ctr.wrapping_add(1);
		}
		self.demote_eof(ep)
	}

	#[inline]
	fn read_single(&mut self, r: &mut &[u8]) -> Result<Option<u8>> {
		let last_read = match r.split_first() {
			Some((v, tail)) => {
				self.ctr = self.ctr.wrapping_add(1);
				*r = tail;
				Some(*v)
			}
			None => {
				if self.has_eof {
					None
				} else {
					return Err(EndOrError::NeedMoreData);
				}
			}
		};
		#[cfg(debug_assertions)]
		{
			self.last_single_read = last_read;
		}
		Ok(last_read)
	}

	#[inline]
	fn skip_matching<B: ByteSelect>(
		&mut self,
		r: &mut &[u8],
		selector: &B,
	) -> (usize, Result<Endbyte>) {
		let (nread, ep) = read::skip_matching_bytes(r, selector);
		self.ctr = self.ctr.wrapping_add(nread);
		match self.demote_eof(ep) {
			Ok(ep) => {
				if let Endbyte::Delimiter(_) = ep {
					self.ctr = self.ctr.wrapping_add(1)
				};
				(nread, Ok(ep))
			}
			Err(e) => (nread, Err(e)),
		}
	}

	fn drop_scratchpad(&mut self) -> Result<()> {
		self.scratchpad.clear();
		Ok(())
	}

	fn swap_scratchpad(&mut self) -> Result<()> {
		core::mem::swap(&mut self.scratchpad, &mut self.swap);
		Ok(())
	}

	fn read_swap(&mut self) -> Vec<u8> {
		let mut tmp = Vec::new();
		core::mem::swap(&mut tmp, &mut self.swap);
		tmp
	}

	fn metrics(&mut self, without: usize) -> TokenMetrics {
		let start = self.last_token_end;
		let end = self.ctr.wrapping_sub(without);
		self.last_token_end = end;
		TokenMetrics { start, end }
	}

	fn flush_scratchpad<U, T: FnOnce(&[u8]) -> Result<U>>(&mut self, conv: T) -> Result<U> {
		let result = conv(&self.scratchpad);
		self.scratchpad.clear();
		result
	}

	fn flush_scratchpad_as_name(&mut self) -> Result<Name> {
		self.flush_scratchpad(|bytes| -> Result<Name> {
			let s = match core::str::from_utf8(bytes) {
				Ok(s) => Ok(s),
				Err(e) => Err(Error::utf8err(bytes, &e)),
			}?;
			Ok(s.try_into()?)
		})
	}

	fn flush_scratchpad_as_complete_cdata(&mut self) -> Result<String> {
		self.flush_scratchpad(|bytes| -> Result<String> {
			let s = match core::str::from_utf8(bytes) {
				Ok(s) => Ok(s),
				Err(e) => Err(Error::utf8err(bytes, &e)),
			}?;
			validate_cdata(s)?;
			Ok(s.to_owned())
		})
	}

	fn flush_scratchpad_as_partial_cdata(&mut self) -> Result<String> {
		let s = match core::str::from_utf8(&self.scratchpad[..]) {
			Ok(s) => s,
			Err(e) => {
				// TODO: this will need refinement...
				let valid_up_to = e.valid_up_to();
				if valid_up_to == 0 {
					// this means that we actually and truly have a broken utf-8 sequence.
					// return an error.
					return Err(EndOrError::Error(Error::InvalidUtf8Byte(
						self.scratchpad[0],
					)));
				} else {
					// okay, we can return the stuff up to here and then let the next call deal with it
					unsafe { core::str::from_utf8_unchecked(&self.scratchpad[..valid_up_to]) }
				}
			}
		};
		validate_cdata(s)?;
		let s = s.to_owned();
		let to_drop = s.len();
		self.scratchpad.drain(..to_drop);
		Ok(s)
	}

	fn maybe_flush_scratchpad_as_text(
		&mut self,
		without: usize,
		complete: bool,
	) -> Result<Option<Token>> {
		if self.scratchpad.is_empty() {
			self.eat_whitespace_metrics(without);
			Ok(None)
		} else {
			Ok(Some(Token::Text(
				self.metrics(without),
				if complete {
					self.flush_scratchpad_as_complete_cdata()?
				} else {
					self.flush_scratchpad_as_partial_cdata()?
				},
			)))
		}
	}

	fn flush_limited_scratchpad_as_text(&mut self) -> Result<Option<Token>> {
		if self.scratchpad.len() >= self.opts.max_token_length
			|| (!self.text_buffering && !self.scratchpad.is_empty())
		{
			Ok(Some(Token::Text(
				self.metrics(0),
				self.flush_scratchpad_as_partial_cdata()?,
			)))
		} else {
			Ok(None)
		}
	}

	/// Interpret a character found inside a text section.
	///
	/// If no interpretation can be found, an Ok result but no next state is
	/// returned.
	///
	/// THIS DOES NOT MEAN THAT THE CHAR IS VALID! IT MAY STILL BE A NUL
	/// BYTE OR SOMESUCH!
	fn lex_posttext_char(&mut self, b: u8) -> Result<Option<ST>> {
		match b {
			b'<' => Ok(Some(ST(
				State::Content(ContentState::MaybeElement(MaybeElementState::Initial)),
				self.maybe_flush_scratchpad_as_text(1, true)?, // 1 == len("<")
			))),
			// begin of forbidden CDATA section end sequence (see XML 1.0 § 2.4 [14])
			b']' => Ok(Some(ST(
				State::Content(ContentState::MaybeCDataEnd(false, 1)),
				// no flush here to avoid needless reallocations on false alarm
				None,
			))),
			b'&' => {
				// We need to be careful here! First, we *have* to swap the scratchpad because that is part of the contract with the Reference state.
				// Second, we have to do this *after* we "maybe" flush the scratchpad as text -- otherwise, we would flush the empty text and then clobber the entity lookup.
				let tok = self.maybe_flush_scratchpad_as_text(1, true)?; // 1 == len("&")
				self.swap_scratchpad()?;
				Ok(Some(ST(
					State::Reference {
						ctx: ErrorContext::Text,
						ret: RefReturnState::Text,
						kind: RefKind::Entity,
					},
					tok,
				)))
			}
			b'\r' => {
				// CRLF needs to be folded to LF, and standalone LF needs, too
				Ok(Some(ST(
					State::Content(ContentState::MaybeCRLF(CrlfReturn::Normal)),
					None,
				)))
			}
			_ => Ok(None),
		}
	}

	fn lex_maybe_element(&mut self, state: MaybeElementState, r: &mut &[u8]) -> Result<ST> {
		match state {
			MaybeElementState::Initial => match self.read_single(r)? {
				Some(byte) => match byte {
					b'?' => {
						self.drop_scratchpad()?;
						Ok(ST(
							State::Content(ContentState::MaybeElement(
								MaybeElementState::XMLDeclStart(2),
							)),
							None,
						))
					}
					b'!' => {
						self.drop_scratchpad()?;
						Ok(ST(
							State::Content(ContentState::MaybeElement(MaybeElementState::Bang(
								Bang::Initial,
							))),
							None,
						))
					}
					b'/' => {
						self.drop_scratchpad()?;
						Ok(ST(
							State::Element {
								kind: ElementKind::Footer,
								state: ElementState::Start,
							},
							None,
						))
					}
					byte => {
						if maybe_name(byte) {
							// add the first character to the scratchpad, because read_single does not do that
							self.prep_scratchpad();
							self.scratchpad.push(byte);
							Ok(ST(
								State::Element {
									kind: ElementKind::Header,
									state: ElementState::Start,
								},
								None,
							))
						} else {
							self.drop_scratchpad()?;
							Err(EndOrError::Error(Error::UnexpectedByte(
								Some(ErrorContext::NameStart),
								byte,
								None,
							)))
						}
					}
				},
				None => Err(EndOrError::Error(Error::wfeof(ErrorContext::Element))),
			},
			MaybeElementState::XMLDeclStart(i) => {
				debug_assert!(i < TOK_XML_DECL_START.len());
				// note: exploiting that xml decl only consists of ASCII here
				let b = handle_eof(self.read_single(r)?, ErrorContext::CdataSectionStart)?;
				if b != TOK_XML_DECL_START[i] {
					return Err(EndOrError::Error(Error::RestrictedXml(
						"processing instructions",
					)));
				}
				let next = i + 1;
				if next == TOK_XML_DECL_START.len() {
					// eliminate the `xml` from the scratchpad
					self.drop_scratchpad()?;
					Ok(ST(
						State::Element {
							kind: ElementKind::XMLDecl,
							state: ElementState::SpaceRequired,
						},
						Some(Token::XMLDeclStart(self.metrics(0))),
					))
				} else {
					Ok(ST(
						State::Content(ContentState::MaybeElement(
							MaybeElementState::XMLDeclStart(next),
						)),
						None,
					))
				}
			}
			MaybeElementState::Bang(Bang::Initial) => {
				let b = handle_eof(self.read_single(r)?, ErrorContext::XmlDeclarationStart)?;
				if b == b'-' {
					Ok(ST(
						State::Content(ContentState::MaybeElement(MaybeElementState::Bang(
							Bang::CommentStart(3),
						))),
						None,
					))
				} else if b == b'[' {
					Ok(ST(
						State::Content(ContentState::MaybeElement(MaybeElementState::Bang(
							Bang::CDataSectionStart(3),
						))),
						None,
					))
				} else {
					return Err(EndOrError::Error(Error::InvalidSyntax(
						"malformed cdata or comment section start",
					)));
				}
			}
			MaybeElementState::Bang(Bang::CDataSectionStart(i)) => {
				debug_assert!(i < TOK_XML_CDATA_START.len());
				let b = handle_eof(self.read_single(r)?, ErrorContext::XmlDeclarationStart)?;
				if b != TOK_XML_CDATA_START[i] {
					return Err(EndOrError::Error(Error::InvalidSyntax(
						"malformed cdata section start",
					)));
				}
				let next = i + 1;
				if next == TOK_XML_CDATA_START.len() {
					self.drop_scratchpad()?;
					Ok(ST(
						State::Content(ContentState::CDataSection),
						self.maybe_flush_scratchpad_as_text(TOK_XML_CDATA_START.len(), true)?,
					))
				} else {
					Ok(ST(
						State::Content(ContentState::MaybeElement(MaybeElementState::Bang(
							Bang::CDataSectionStart(next),
						))),
						None,
					))
				}
			}
			MaybeElementState::Bang(Bang::CommentStart(i)) => {
				debug_assert!(i < TOK_XML_COMMENT_START.len());
				let b = handle_eof(self.read_single(r)?, ErrorContext::XmlDeclarationStart)?;
				if b != TOK_XML_COMMENT_START[i] {
					return Err(EndOrError::Error(Error::InvalidSyntax(
						"malformed cdata section start",
					)));
				}
				let next = i + 1;
				if next == TOK_XML_COMMENT_START.len() {
					self.drop_scratchpad()?;
					Ok(ST(
						State::Content(ContentState::Comment),
						Some(Token::CommentStart(self.metrics(0))),
					))
				} else {
					Ok(ST(
						State::Content(ContentState::MaybeElement(MaybeElementState::Bang(
							Bang::CommentStart(next),
						))),
						None,
					))
				}
			}
		}
	}

	fn lex_resume_text(&mut self, b: u8) -> Result<ST> {
		match self.lex_posttext_char(b)? {
			// special delimiter char -> state transition
			Some(st) => Ok(st),
			// no special char -> check if it is possibly valid text and proceed accordingly
			None => {
				if is_nonchar_byte(b) {
					// non-Char, error
					Err(EndOrError::Error(Error::InvalidChar(
						Some(ErrorContext::Text),
						b as u32,
						false,
					)))
				} else {
					// nothing special, push to scratchpad and return to initial content state
					self.prep_scratchpad();
					self.scratchpad.push(b);
					Ok(ST(State::Content(ContentState::Initial), None))
				}
			}
		}
	}

	fn lex_maybe_cdata_end(&mut self, in_cdata: bool, nend: usize, r: &mut &[u8]) -> Result<ST> {
		debug_assert!(nend < TOK_XML_CDATA_END.len());
		let ctx = if in_cdata {
			ErrorContext::CdataSection
		} else {
			ErrorContext::Text
		};
		let b = handle_eof(self.read_single(r)?, ctx)?;
		let expected = TOK_XML_CDATA_END[nend];
		if b == expected {
			// sequence continues
			match nend {
				1 => Ok(ST(
					State::Content(ContentState::MaybeCDataEnd(in_cdata, 2)),
					None,
				)),
				// ]]> read completely! Do something!
				2 => {
					if !in_cdata {
						// ]]> is forbidden outside CDATA sections -> error
						Err(EndOrError::Error(Error::InvalidSyntax(
							"unescaped ']]>' forbidden in text",
						)))
					} else {
						// we are inside the cdata section and the previous char we read was the last byte of the closing delimiter
						// this means that we can safely exit without interpreting the char.
						// and we must not subtract this char, because it is part of the CDATA section
						Ok(ST(
							State::Content(ContentState::Initial),
							self.maybe_flush_scratchpad_as_text(0, true)?,
						))
					}
				}
				_ => panic!("unreachable state: cdata nend = {:?}", nend),
			}
		} else if b == b']' {
			// sequence was broken, but careful! this could just be `]]]]]]]>` sequence!
			// those we need to treat the same, no matter whether inside or outside CDATA the previously found ] is moved to the scratchpad and we return to this state
			self.prep_scratchpad();
			self.scratchpad.push(b']');
			Ok(ST(
				State::Content(ContentState::MaybeCDataEnd(in_cdata, nend)),
				self.flush_limited_scratchpad_as_text()?,
			))
		} else {
			// sequence was broken
			self.prep_scratchpad();
			self.scratchpad
				.extend_from_slice(&TOK_XML_CDATA_END[..nend]);
			if in_cdata {
				if is_nonchar_byte(b) {
					// that’s a sneaky one!
					Err(EndOrError::Error(Error::InvalidChar(
						Some(ErrorContext::CdataSection),
						b as u32,
						false,
					)))
				} else {
					// broken sequence inside cdata section, that’s fine; just push whatever we read to the scratchpad and move on
					// no need for prep, we pushed above already
					self.scratchpad.push(b);
					Ok(ST(
						State::Content(ContentState::CDataSection),
						// enforce token size limits here, too
						self.flush_limited_scratchpad_as_text()?,
					))
				}
			} else {
				// broken sequence outside cdata section, need to analyze the next char carefully to handle entities and such
				self.lex_resume_text(b)
			}
		}
	}

	fn lex_maybe_comment_end(&mut self, nend: usize, r: &mut &[u8]) -> Result<ST> {
		debug_assert!(nend < TOK_XML_COMMENT_END.len());
		let b = handle_eof(self.read_single(r)?, ErrorContext::Comment)?;
		let expected = TOK_XML_COMMENT_END[nend];
		if b == expected {
			// sequence continues
			return match nend {
				// `--` read. We can flush the scratchpad here, because either
				// we get a valid comment section (`--` followed by `>`) or we
				// get a syntax error.
				// this simplifies things, because we don't have to worry
				// about emitting two tokens at `>`.
				1 => Ok(ST(
					State::Content(ContentState::MaybeCommentEnd(2)),
					// we do subtract the `--` from the metrics though, because
					// we want that counted against the `-->` token.
					self.maybe_flush_scratchpad_as_text(2, true)?,
				)),
				// `-->` read completely
				2 => {
					// the comment is now closed, so we emit that token and
					// return to the initial state.
					Ok(ST(
						State::Content(ContentState::Initial),
						Some(Token::CommentEnd(self.metrics(0))),
					))
				}
				_ => panic!("unreachable state: comment nend = {:?}", nend),
			};
		}

		if nend == 1 {
			// we only read `-` so far, so this is still legal. add it to
			// the scratchpad and return to the comment state.
			self.prep_scratchpad();
			self.scratchpad.push(b'-');
			if is_nonchar_byte(b) {
				// that’s a sneaky one!
				Err(EndOrError::Error(Error::InvalidChar(
					Some(ErrorContext::Comment),
					b as u32,
					false,
				)))
			} else {
				self.scratchpad.push(b);
				Ok(ST(
					State::Content(ContentState::Comment),
					// enforce token size limits here, too
					self.flush_limited_scratchpad_as_text()?,
				))
			}
		} else {
			// we read `--` and the next char was not a `>`. This is not
			// allowed.
			Err(EndOrError::Error(Error::InvalidSyntax("`--` in comment")))
		}
	}

	fn lex_content(&mut self, state: ContentState, r: &mut &[u8]) -> Result<ST> {
		match state {
			ContentState::MaybeElement(substate) => self.lex_maybe_element(substate, r),
			ContentState::MaybeCDataEnd(in_cdata, nend) => {
				self.lex_maybe_cdata_end(in_cdata, nend, r)
			}
			ContentState::MaybeCommentEnd(nend) => self.lex_maybe_comment_end(nend, r),

			ContentState::MaybeCRLF(return_to) => {
				let b = handle_eof(self.read_single(r)?, ErrorContext::Text)?;
				match b {
					b'\n' => {
						// CRLF sequence, only insert the \n to the scratchpad.
						self.prep_scratchpad();
						self.scratchpad.push(b'\n');
						// return to the content state and curse a bit
						Ok(ST(State::Content(return_to.into()), None))
					}
					b'\r' => {
						// double CR, so this may still be followed by an LF; but the first CR gets converted to LF
						self.prep_scratchpad();
						self.scratchpad.push(b'\n');
						// stay in the same state, we may still get an LF here.
						Ok(ST(State::Content(ContentState::MaybeCRLF(return_to)), None))
					}
					b => {
						// we read a single CR, so we push a \n to the scratchpad and hope for the best
						self.prep_scratchpad();
						self.scratchpad.push(b'\n');
						match return_to {
							CrlfReturn::Normal => self.lex_resume_text(b),
							CrlfReturn::Cdata => {
								// only special thing in CDATA is ']'
								if b == b']' {
									Ok(ST(
										State::Content(ContentState::MaybeCDataEnd(true, 1)),
										None,
									))
								} else if !is_nonchar_byte(b) {
									// ^ but of course we still need to check for a valid char. Thanks afl.
									// no need for prep as we pushed above already
									self.scratchpad.push(b);
									Ok(ST(State::Content(ContentState::CDataSection), None))
								} else {
									Err(EndOrError::Error(Error::InvalidChar(
										Some(ErrorContext::CdataSection),
										b as u32,
										false,
									)))
								}
							}
							CrlfReturn::Comment => {
								// only special thing in comments is '-'
								if b == b'-' {
									Ok(ST(State::Content(ContentState::MaybeCommentEnd(1)), None))
								} else if !is_nonchar_byte(b) {
									// ^ but of course we still need to check for a valid char.
									// no need for prep as we pushed above already
									self.scratchpad.push(b);
									Ok(ST(State::Content(ContentState::Comment), None))
								} else {
									Err(EndOrError::Error(Error::InvalidChar(
										Some(ErrorContext::Comment),
										b as u32,
										false,
									)))
								}
							}
						}
					}
				}
			}

			// read until next `<` or `&`, which are the only things which
			// can break us out of this state.
			ContentState::Initial => {
				match self.read_validated(r, &maybe_text, self.opts.max_token_length) {
					Ok(Endbyte::Eof) => Ok(ST(
						State::Eof,
						self.maybe_flush_scratchpad_as_text(0, true)?,
					)),
					Ok(Endbyte::Limit) => Ok(ST(
						State::Content(ContentState::Initial),
						self.maybe_flush_scratchpad_as_text(0, false)?,
					)),
					Ok(Endbyte::Delimiter(b)) => match self.lex_posttext_char(b)? {
						Some(st) => Ok(st),
						// not a "special" char but not text either -> error
						None => Err(EndOrError::Error(Error::InvalidChar(
							Some(ErrorContext::Text),
							b as u32,
							false,
						))),
					},
					Err(EndOrError::NeedMoreData)
						if !self.text_buffering && !self.scratchpad.is_empty() =>
					{
						Ok(ST(
							State::Content(ContentState::Initial),
							// Because eager text emission may cause us to see
							// a partial UTF-8 sequence at the beginning of
							// the buffer, we have to be lenient here.
							match self.maybe_flush_scratchpad_as_text(0, false) {
								// Only ignore UTF-8 errors if the scratchpad
								// is too short to contain a full UTF-8
								// sequence. Any broken sequence in a
								// scratchpad which is at least 4 bytes long
								// is indeed fatal.
								Err(EndOrError::Error(Error::InvalidUtf8Byte(..)))
									if self.scratchpad.len() < 4 =>
								{
									return Err(EndOrError::NeedMoreData);
								}
								Err(other) => return Err(other),
								Ok(v) => v,
							},
						))
					}
					Err(other) => Err(other),
				}
			}
			ContentState::CDataSection => {
				match self.read_validated(r, &maybe_cdata_content, self.opts.max_token_length)? {
					Endbyte::Eof => {
						Err(EndOrError::Error(Error::wfeof(ErrorContext::CdataSection)))
					}
					Endbyte::Limit => Ok(ST(
						State::Content(ContentState::CDataSection),
						self.maybe_flush_scratchpad_as_text(0, false)?,
					)),
					Endbyte::Delimiter(b) => match b {
						// -> transition into the "first delimiter found" state
						b']' => Ok(ST(
							State::Content(ContentState::MaybeCDataEnd(true, 1)),
							None,
						)),
						b'\r' => Ok(ST(
							State::Content(ContentState::MaybeCRLF(CrlfReturn::Cdata)),
							None,
						)),
						_ => Err(EndOrError::Error(Error::InvalidChar(
							Some(ErrorContext::CdataSection),
							b as u32,
							false,
						))),
					},
				}
			}
			ContentState::Whitespace => match self.skip_matching(r, &is_space) {
				(_, Ok(Endbyte::Eof)) | (_, Ok(Endbyte::Limit)) => Ok(ST(State::Eof, None)),
				(_, Ok(Endbyte::Delimiter(b))) => match b {
					b'<' => Ok(ST(
						State::Content(ContentState::MaybeElement(MaybeElementState::Initial)),
						None,
					)),
					_ => Err(EndOrError::Error(Error::UnexpectedByte(
						Some(ErrorContext::XmlDeclarationEnd),
						b,
						Some(&["Spaces", "<"]),
					))),
				},
				(_, Err(e)) => Err(e),
			},

			ContentState::Comment => {
				match self.read_validated(r, &maybe_comment_content, self.opts.max_token_length)? {
					Endbyte::Eof => Err(EndOrError::Error(Error::wfeof(ErrorContext::Comment))),
					Endbyte::Limit => Ok(ST(
						State::Content(ContentState::Comment),
						self.maybe_flush_scratchpad_as_text(0, false)?,
					)),
					Endbyte::Delimiter(b) => match b {
						// -> transition into the "first delimiter found" state
						b'-' => Ok(ST(State::Content(ContentState::MaybeCommentEnd(1)), None)),
						b'\r' => Ok(ST(
							State::Content(ContentState::MaybeCRLF(CrlfReturn::Comment)),
							None,
						)),
						_ => Err(EndOrError::Error(Error::InvalidChar(
							Some(ErrorContext::CdataSection),
							b as u32,
							false,
						))),
					},
				}
			}
		}
	}

	fn lex_element_postblank(&mut self, kind: ElementKind, b: u8) -> Result<ElementState> {
		match b {
			b' ' | b'\t' | b'\r' | b'\n' => Ok(ElementState::Blank),
			b'"' => Ok(ElementState::AttributeValue(b'"', false)),
			b'\'' => Ok(ElementState::AttributeValue(b'\'', false)),
			b'=' => Ok(ElementState::Eq),
			b'>' => match kind {
				ElementKind::Footer | ElementKind::Header => Ok(ElementState::Close),
				ElementKind::XMLDecl => Err(EndOrError::Error(Error::UnexpectedChar(
					Some(ErrorContext::XmlDeclaration),
					'>',
					Some(&["?"]),
				))),
			},
			b'?' => match kind {
				ElementKind::XMLDecl => Ok(ElementState::MaybeXMLDeclEnd),
				_ => Err(EndOrError::Error(Error::UnexpectedChar(
					Some(ErrorContext::Element),
					'?',
					None,
				))),
			},
			b'/' => match kind {
				ElementKind::Header => Ok(ElementState::MaybeHeadClose),
				ElementKind::Footer => Err(EndOrError::Error(Error::UnexpectedChar(
					Some(ErrorContext::ElementFoot),
					'/',
					None,
				))),
				ElementKind::XMLDecl => Err(EndOrError::Error(Error::UnexpectedChar(
					Some(ErrorContext::XmlDeclaration),
					'/',
					None,
				))),
			},
			b if maybe_name(b) => {
				// write the char to scratchpad because it’ll be needed.
				self.prep_scratchpad();
				self.scratchpad.push(b);
				Ok(ElementState::Name)
			}
			_ => Err(EndOrError::Error(Error::UnexpectedByte(
				match kind {
					ElementKind::XMLDecl => Some(ErrorContext::XmlDeclaration),
					_ => Some(ErrorContext::Element),
				},
				b,
				Some(&["whitespace", "\"", "'", "=", ">", "?", "/", "start of name"]),
			))),
		}
	}

	fn lex_attval_next(&mut self, delim: u8, b: u8, element_kind: ElementKind) -> Result<ST> {
		match b {
			b'<' => Err(EndOrError::Error(Error::UnexpectedChar(
				Some(ErrorContext::AttributeValue),
				'<',
				None,
			))),
			b'&' => {
				// must swap scratchpad here to avoid clobbering the
				// attribute value during entity read
				self.swap_scratchpad()?;
				Ok(ST(
					State::Reference {
						ctx: ErrorContext::AttributeValue,
						ret: RefReturnState::AttributeValue(element_kind, delim),
						kind: RefKind::Entity,
					},
					None,
				))
			}
			b'\t' | b'\n' => {
				self.prep_scratchpad();
				self.scratchpad.push(b' ');
				Ok(ST(
					State::Element {
						kind: element_kind,
						state: ElementState::AttributeValue(delim, false),
					},
					None,
				))
			}
			b'\r' => Ok(ST(
				State::Element {
					kind: element_kind,
					state: ElementState::AttributeValue(delim, true),
				},
				None,
			)),
			d if d == delim => Ok(ST(
				State::Element {
					kind: element_kind,
					// require whitespace after attribute as the grammar demands
					state: ElementState::SpaceRequired,
				},
				Some(Token::AttributeValue(
					self.metrics(0),
					self.flush_scratchpad_as_complete_cdata()?,
				)),
			)),
			other => Err(EndOrError::Error(Error::InvalidChar(
				Some(ErrorContext::AttributeValue),
				other as u32,
				false,
			))),
		}
	}

	fn lex_element(&mut self, kind: ElementKind, state: ElementState, r: &mut &[u8]) -> Result<ST> {
		match state {
			ElementState::Start | ElementState::Name => {
				match self.read_validated(r, &maybe_name, self.opts.max_token_length)? {
					Endbyte::Eof => Err(EndOrError::Error(Error::wfeof(ErrorContext::Name))),
					Endbyte::Limit => Err(Self::token_length_error()),
					Endbyte::Delimiter(ch) => {
						let next_state = self.lex_element_postblank(kind, ch)?;
						let name =
							add_context(self.flush_scratchpad_as_name(), ErrorContext::NameStart)?;
						let metrics = self.metrics(1);
						Ok(ST(
							State::Element {
								kind,
								state: next_state,
							},
							Some(if state == ElementState::Name {
								Token::Name(metrics, name)
							} else {
								match kind {
									ElementKind::Header => Token::ElementHeadStart(metrics, name),
									ElementKind::Footer => Token::ElementFootStart(metrics, name),
									ElementKind::XMLDecl => panic!("invalid state"),
								}
							}),
						))
					}
				}
			}
			ElementState::SpaceRequired | ElementState::Blank => {
				match self.skip_matching(r, &is_space) {
					(_, Ok(Endbyte::Eof)) | (_, Ok(Endbyte::Limit)) => {
						Err(EndOrError::Error(Error::wfeof(ErrorContext::Element)))
					}
					(nmatching, Err(EndOrError::NeedMoreData))
						if nmatching > 0 && state == ElementState::SpaceRequired =>
					{
						// we have to treat IO errors here specially and implicitly retry them (because we swallow this one). that is in line with the contract which says that IO errors are retriable
						// this is because we need to transition from SpaceRequired to Blank after reading even only a single char. otherwise, we are not resilient against chunking.
						Ok(ST(
							State::Element {
								kind,
								state: ElementState::Blank,
							},
							None,
						))
					}
					(nmatching, Ok(Endbyte::Delimiter(b))) => {
						self.eat_whitespace_metrics(1);
						let next_state = self.lex_element_postblank(kind, b)?;
						if next_state == ElementState::Name
							&& state == ElementState::SpaceRequired
							&& nmatching == 0
						{
							Err(EndOrError::Error(Error::InvalidSyntax(
								"space required before attribute names",
							)))
						} else {
							Ok(ST(
								State::Element {
									kind,
									state: next_state,
								},
								None,
							))
						}
					}
					(_, Err(e)) => Err(e),
				}
			}
			// XML 1.0 §2.3 [10] AttValue
			ElementState::AttributeValue(delim, false) => {
				let selector = if delim == b'\'' {
					&maybe_attval_apos as &dyn Fn(_) -> _
				} else {
					&maybe_attval_quot as &dyn Fn(_) -> _
				};
				match self.read_validated(r, &selector, self.opts.max_token_length)? {
					Endbyte::Eof => Err(EndOrError::Error(Error::wfeof(
						ErrorContext::AttributeValue,
					))),
					Endbyte::Limit => Err(Self::token_length_error()),
					Endbyte::Delimiter(utf8ch) => self.lex_attval_next(delim, utf8ch, kind),
				}
			}
			// CRLF normalization for attributes; cannot reuse the element mechanism here because we have to carry around the delimiter and stuff
			ElementState::AttributeValue(delim, true) => {
				let b = handle_eof(self.read_single(r)?, ErrorContext::AttributeValue)?;
				if b == b'\r' {
					// push the space, continue with CRLF
					self.prep_scratchpad();
					self.scratchpad.push(b' ');
					Ok(ST(
						State::Element {
							kind,
							state: ElementState::AttributeValue(delim, true),
						},
						None,
					))
				} else {
					// not another CR, so we can move on to the default handling
					self.lex_attval_next(delim, b, kind)
				}
			}
			ElementState::MaybeXMLDeclEnd => match self.read_single(r)? {
				Some(b'>') => {
					self.drop_scratchpad()?;
					Ok(ST(
						State::Content(ContentState::Whitespace),
						Some(Token::XMLDeclEnd(self.metrics(0))),
					))
				}
				Some(b) => Err(EndOrError::Error(Error::UnexpectedByte(
					Some(ErrorContext::XmlDeclarationEnd),
					b,
					Some(&[">"]),
				))),
				None => Err(EndOrError::Error(Error::wfeof(
					ErrorContext::XmlDeclarationEnd,
				))),
			},
			ElementState::MaybeHeadClose => match self.read_single(r)? {
				Some(b'>') => {
					self.drop_scratchpad()?;
					Ok(ST(
						State::Content(ContentState::Initial),
						Some(Token::ElementHeadClose(self.metrics(0))),
					))
				}
				Some(b) => Err(EndOrError::Error(Error::UnexpectedByte(
					Some(ErrorContext::ElementClose),
					b,
					Some(&[">"]),
				))),
				None => Err(EndOrError::Error(Error::wfeof(ErrorContext::ElementClose))),
			},
			// do NOT read anything here; this state is entered when
			// another state has read a '='. We can always transition to
			// Blank afterward, as that will read the next char and decide
			// (and potentially scratchpad) correctly.
			ElementState::Eq => Ok(ST(
				State::Element {
					kind,
					state: ElementState::Blank,
				},
				Some(Token::Eq(self.metrics(0))),
			)),
			// like with Eq, no read here
			ElementState::Close => Ok(ST(
				State::Content(ContentState::Initial),
				Some(Token::ElementHFEnd(self.metrics(0))),
			)),
		}
	}

	fn lex_reference(
		&mut self,
		ctx: ErrorContext,
		ret: RefReturnState,
		kind: RefKind,
		r: &mut &[u8],
	) -> Result<ST> {
		let result = match kind {
			RefKind::Entity => self.read_validated(r, &maybe_name, MAX_REFERENCE_LENGTH)?,
			RefKind::Char(CharRefRadix::Decimal) => {
				self.read_validated(r, &is_decimal_digit, MAX_REFERENCE_LENGTH)?
			}
			RefKind::Char(CharRefRadix::Hexadecimal) => {
				self.read_validated(r, &is_hexadecimal_digit, MAX_REFERENCE_LENGTH)?
			}
		};
		let result = match result {
			Endbyte::Eof => return Err(EndOrError::Error(Error::wfeof(ErrorContext::Reference))),
			Endbyte::Limit => return Err(EndOrError::Error(Error::UndeclaredEntity)),
			Endbyte::Delimiter(b) => match b {
				b'#' => {
					if !self.scratchpad.is_empty() {
						Err(b'#')
					} else {
						match kind {
							RefKind::Entity => {
								return Ok(ST(
									State::Reference {
										ctx,
										ret,
										kind: RefKind::Char(CharRefRadix::Decimal),
									},
									None,
								));
							}
							_ => Err(b'#'),
						}
					}
				}
				b'x' => {
					if !self.scratchpad.is_empty() {
						Err(b'x')
					} else {
						match kind {
							RefKind::Char(CharRefRadix::Decimal) => {
								return Ok(ST(
									State::Reference {
										ctx,
										ret,
										kind: RefKind::Char(CharRefRadix::Hexadecimal),
									},
									None,
								));
							}
							_ => Err(b'x'),
						}
					}
				}
				b';' => {
					if self.scratchpad.is_empty() {
						return Err(EndOrError::Error(Error::InvalidSyntax("empty reference")));
					}
					// return to main scratchpad
					self.swap_scratchpad()?;
					// the entity reference is now in the swap (which we have to clear now, too)
					let entity = self.read_swap();
					match kind {
						RefKind::Entity => {
							let b = add_context(resolve_named_entity(&entity[..]), ctx)?;
							self.scratchpad.push(b);
							Ok(())
						}
						RefKind::Char(radix) => {
							// this is safe because the bytes allowed by the digit byte ranges are all plain ascii
							let entity = unsafe { core::str::from_utf8_unchecked(&entity[..]) };
							Ok(add_context(
								resolve_char_reference(entity, radix, &mut self.scratchpad),
								ctx,
							)?)
						}
					}
				}
				c => Err(c),
			},
		};
		match result {
			Ok(_) => Ok(ST(ret.to_state(), None)),
			Err(b) => Err(EndOrError::Error(Error::UnexpectedByte(
				Some(ErrorContext::Reference),
				b,
				Some(&[";"]),
			))),
		}
	}

	fn lex_bytes_raw(&mut self, r: &mut &[u8]) -> Result<Option<Token>> {
		if let Some(e) = self.err {
			return Err(EndOrError::Error(e));
		}

		loop {
			let stresult = match self.state {
				State::Content(substate) => self.lex_content(substate, r),
				State::Element {
					kind,
					state: substate,
				} => self.lex_element(kind, substate, r),
				State::Reference { ctx, ret, kind } => self.lex_reference(ctx, ret, kind, r),
				State::Eof => return Ok(None),
			};
			let st = match stresult {
				Err(EndOrError::NeedMoreData) => return Err(EndOrError::NeedMoreData),
				Err(EndOrError::Error(other)) => {
					// we cache all other errors because we don't want to read / emit invalid data
					self.err = Some(other);
					return Err(EndOrError::Error(other));
				}
				Ok(st) => st,
			};
			if let Some(tok) = st.splice(&mut self.state) {
				#[cfg(debug_assertions)]
				{
					// preserve the state for infinite loop detection
					self.prev_state = (self.scratchpad.clone(), self.state);
				}
				return Ok(Some(tok));
			}
			#[cfg(debug_assertions)]
			{
				// we did not leave the loop; assert that the state has
				// actually changed
				if self.prev_state.0 == self.scratchpad && self.prev_state.1 == self.state {
					panic!(
						"state has not changed in the last iteration: {:?} {:?} last read: {:?}",
						self, self.scratchpad, self.last_single_read
					)
				} else {
					self.prev_state = (self.scratchpad.clone(), self.state)
				}
			}
		}
	}

	/// Lex bytes from the buffer, advancing the slice for any byte consumed,
	/// until either an error occurs, a valid token is produced or the buffer
	/// is at its end.
	///
	/// **Note:** The lexer keeps some internal state which may cause a token
	/// to be emitted even for an empty buffer! That means that even if your
	/// backend currently has no more data available, you should call
	/// `lex_bytes` with a corresponding empty buffer and eof flag until you
	/// receive a non-token result.
	///
	/// # End-of-file handling
	///
	/// The `Lexer` can be used to process a streamed document. For this, it
	/// needs to know whether the end of the buffer passed to this function
	/// marks the end of the document or not. The caller signals this using
	/// the `at_eof` flag.
	///
	/// If `at_eof` is false, the end of buffer is treated as a temporary
	/// situation and a [`std::io::ErrorKind::WouldBlock`] I/O error is
	/// returned when it is reached. Otherwise, the end of buffer is treated
	/// as the end of file.
	///
	/// # Return value
	///
	/// Returns `None` if a valid end of file is reached, a token if a valid
	/// token is encountered or an error otherwise.
	pub(crate) fn lex_bytes(&mut self, r: &mut &[u8], at_eof: bool) -> Result<Option<Token>> {
		self.has_eof = at_eof;
		self.lex_bytes_raw(r)
	}

	/// Release all temporary buffers
	///
	/// This is sensible to call when it is expected that no more data will be
	/// processed by the lexer for a while and the memory is better used
	/// elsewhere.
	pub fn release_temporaries(&mut self) {
		self.scratchpad.shrink_to_fit();
		self.swap.shrink_to_fit();
	}
}

impl fmt::Debug for Lexer {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Lexer").field("state", &self.state).finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::format;
	use alloc::string::ToString;
	use std::io;

	trait Sink {
		type ErrorType;

		fn token(&mut self, token: Token);
	}

	/// Stream tokens to the sink until the end of stream is reached.
	fn stream_to_sink<S: Sink>(
		l: &mut Lexer,
		r: &mut &[u8],
		s: &mut S,
		at_eof: bool,
	) -> Result<()> {
		loop {
			match l.lex_bytes(r, at_eof)? {
				Some(tok) => s.token(tok),
				None => break,
			}
		}
		Ok(())
	}

	fn stream_to_sink_from_bytes<S: Sink>(l: &mut Lexer, r: &mut &[u8], s: &mut S) -> Result<()> {
		loop {
			match l.lex_bytes(r, true)? {
				Some(tok) => s.token(tok),
				None => break,
			}
		}
		Ok(())
	}

	struct VecSink {
		dest: Vec<Token>,
		limit: usize,
	}

	impl VecSink {
		fn new(limit: usize) -> VecSink {
			VecSink {
				dest: Vec::new(),
				limit: limit,
			}
		}
	}

	impl Sink for VecSink {
		type ErrorType = io::Error;

		fn token(&mut self, token: Token) {
			if self.dest.len() >= self.limit {
				panic!("token limit exceeded: {}", self.limit);
			}
			self.dest.push(token);
		}
	}

	fn new_lexer() -> Lexer {
		Lexer::new(crate::parser::Options::default().into())
	}

	fn lex(mut data: &[u8], token_limit: usize) -> (Vec<Token>, Result<()>) {
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(token_limit);
		let result = stream_to_sink(&mut lexer, &mut data, &mut sink, true);
		(sink.dest, result)
	}

	fn lex_chunked_with(
		data: &[&[u8]],
		token_limit: usize,
		mut lexer: Lexer,
	) -> (Vec<Token>, Result<()>) {
		let mut sink = VecSink::new(token_limit);
		for chunk in data.iter() {
			let mut chunk = *chunk;
			match stream_to_sink(&mut lexer, &mut chunk, &mut sink, false) {
				Ok(()) => panic!("unexpected end of tokens"),
				Err(EndOrError::NeedMoreData) => (),
				Err(e) => return (sink.dest, Err(e)),
			}
			assert_eq!(chunk.len(), 0);
		}
		let result = stream_to_sink(&mut lexer, &mut &[][..], &mut sink, true);
		(sink.dest, result)
	}

	fn lex_chunked(data: &[&[u8]], token_limit: usize) -> (Vec<Token>, Result<()>) {
		let lexer = new_lexer();

		lex_chunked_with(data, token_limit, lexer)
	}

	fn lex_err(data: &[u8], token_limit: usize) -> Option<EndOrError> {
		let (_, r) = lex(data, token_limit);
		r.err()
	}

	fn run_fuzz_test(mut data: &[u8], token_limit: usize) -> Result<Vec<Token>> {
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(token_limit);
		stream_to_sink(&mut lexer, &mut data, &mut sink, true)?;
		Ok(sink.dest)
	}

	#[test]
	fn lexer_lex_xml_decl_start() {
		let mut src = "<?xml".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();

		assert_eq!(
			sink.dest[0],
			Token::XMLDeclStart(TokenMetrics { start: 0, end: 5 })
		);
	}

	#[test]
	fn lexer_lex_rejects_invalid_xml_decl_opener() {
		let mut src = "<?xmlversion".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		let err = stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();
		assert!(!matches!(err, EndOrError::Error(Error::InvalidEof(..))));

		assert_eq!(
			sink.dest[0],
			Token::XMLDeclStart(TokenMetrics { start: 0, end: 5 })
		);
		assert_eq!(sink.dest.len(), 1);
	}

	#[test]
	fn lexer_lex_xml_decl_version_name() {
		let mut src = "<?xml version=".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();

		assert_eq!(
			sink.dest[1],
			Token::Name(
				TokenMetrics { start: 6, end: 13 },
				"version".try_into().unwrap()
			)
		);
	}

	#[test]
	fn lexer_lex_xml_decl_version_eq() {
		let mut src = "<?xml version=".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();

		assert_eq!(sink.dest[2], Token::Eq(TokenMetrics { start: 13, end: 14 }));
	}

	#[test]
	fn lexer_lex_xml_decl_version_value_squot() {
		let mut src = "<?xml version='1.0'".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();

		assert_eq!(
			sink.dest[3],
			Token::AttributeValue(
				TokenMetrics { start: 14, end: 19 },
				"1.0".try_into().unwrap()
			)
		);
	}

	#[test]
	fn lexer_lex_xml_decl_version_value_dquot() {
		let mut src = "<?xml version=\"1.0\"".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();

		assert_eq!(
			sink.dest[3],
			Token::AttributeValue(
				TokenMetrics { start: 14, end: 19 },
				"1.0".try_into().unwrap()
			)
		);
	}

	#[test]
	fn lexer_lex_xml_decl_end() {
		let mut src = "<?xml version=\"1.0\"?>".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		assert_eq!(
			sink.dest[4],
			Token::XMLDeclEnd(TokenMetrics { start: 19, end: 21 })
		);
	}

	#[test]
	fn lexer_lex_xml_decl_complete() {
		let mut src = "<?xml version=\"1.0\" encoding='utf-8'?>".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		let result = stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink);

		assert!(result.is_ok());
		assert_eq!(
			sink.dest[0],
			Token::XMLDeclStart(TokenMetrics { start: 0, end: 5 })
		);
		assert_eq!(
			sink.dest[1],
			Token::Name(
				TokenMetrics { start: 6, end: 13 },
				"version".try_into().unwrap()
			)
		);
		assert_eq!(sink.dest[2], Token::Eq(TokenMetrics { start: 13, end: 14 }));
		assert_eq!(
			sink.dest[3],
			Token::AttributeValue(
				TokenMetrics { start: 14, end: 19 },
				"1.0".try_into().unwrap()
			)
		);
		assert_eq!(
			sink.dest[4],
			Token::Name(
				TokenMetrics { start: 20, end: 28 },
				"encoding".try_into().unwrap()
			)
		);
		assert_eq!(sink.dest[5], Token::Eq(TokenMetrics { start: 28, end: 29 }));
		assert_eq!(
			sink.dest[6],
			Token::AttributeValue(
				TokenMetrics { start: 29, end: 36 },
				"utf-8".try_into().unwrap()
			)
		);
		assert_eq!(
			sink.dest[7],
			Token::XMLDeclEnd(TokenMetrics { start: 36, end: 38 })
		);
	}

	#[test]
	fn lexer_lex_element_start() {
		let mut src = &b"<element "[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();

		assert_eq!(
			sink.dest[0],
			Token::ElementHeadStart(
				TokenMetrics { start: 0, end: 8 },
				"element".try_into().unwrap()
			)
		);
	}

	#[test]
	fn lexer_lex_element_noattr_empty() {
		let mut src = &b"<element/>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		assert_eq!(
			sink.dest[0],
			Token::ElementHeadStart(
				TokenMetrics { start: 0, end: 8 },
				"element".try_into().unwrap()
			)
		);
		assert_eq!(
			sink.dest[1],
			Token::ElementHeadClose(TokenMetrics { start: 8, end: 10 })
		);
	}

	#[test]
	fn lexer_lex_element_noattr_open() {
		let mut src = &b"<element>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		assert_eq!(
			sink.dest[0],
			Token::ElementHeadStart(
				TokenMetrics { start: 0, end: 8 },
				"element".try_into().unwrap()
			)
		);
		assert_eq!(
			sink.dest[1],
			Token::ElementHFEnd(TokenMetrics { start: 8, end: 9 })
		);
	}

	#[test]
	fn lexer_lex_element_noattr_empty_explicit() {
		let mut src = &b"<element></element>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		assert_eq!(
			sink.dest[0],
			Token::ElementHeadStart(
				TokenMetrics { start: 0, end: 8 },
				"element".try_into().unwrap()
			)
		);
		assert_eq!(
			sink.dest[1],
			Token::ElementHFEnd(TokenMetrics { start: 8, end: 9 })
		);
		assert_eq!(
			sink.dest[2],
			Token::ElementFootStart(
				TokenMetrics { start: 9, end: 18 },
				"element".try_into().unwrap()
			)
		);
		assert_eq!(
			sink.dest[3],
			Token::ElementHFEnd(TokenMetrics { start: 18, end: 19 })
		);
	}

	#[test]
	fn lexer_lex_element_attribute() {
		let mut src = &b"<element x='foo' y=\"bar\" xmlns='baz' xmlns:abc='fnord'>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		assert!(matches!(iter.next().unwrap(), Token::ElementHeadStart(_, nm) if nm == "element"));
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(TokenMetrics { start: 9, end: 10 }, "x".try_into().unwrap())
		);
		assert!(matches!(iter.next().unwrap(), Token::Eq(_)));
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 11, end: 16 },
				"foo".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(TokenMetrics { start: 17, end: 18 }, "y".try_into().unwrap())
		);
		assert!(matches!(iter.next().unwrap(), Token::Eq(_)));
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 19, end: 24 },
				"bar".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(
				TokenMetrics { start: 25, end: 30 },
				"xmlns".try_into().unwrap()
			)
		);
		assert!(matches!(iter.next().unwrap(), Token::Eq(_)));
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 31, end: 36 },
				"baz".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(
				TokenMetrics { start: 37, end: 46 },
				"xmlns:abc".try_into().unwrap()
			)
		);
		assert!(matches!(iter.next().unwrap(), Token::Eq(_)));
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 47, end: 54 },
				"fnord".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 54, end: 55 })
		);
	}

	#[test]
	fn lexer_lex_text() {
		let mut src = &b"<root>Hello World!</root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		assert!(matches!(iter.next().unwrap(), Token::ElementHeadStart(_, nm) if nm == "root"));
		assert!(matches!(iter.next().unwrap(), Token::ElementHFEnd(_)));
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 6, end: 18 },
				"Hello World!".try_into().unwrap()
			)
		);
		assert!(matches!(iter.next().unwrap(), Token::ElementFootStart(_, nm) if nm == "root"));
		assert!(matches!(iter.next().unwrap(), Token::ElementHFEnd(_)));
	}

	#[test]
	fn lexer_lex_amp() {
		let mut src = &b"<root>&amp;</root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		assert!(matches!(iter.next().unwrap(), Token::ElementHeadStart(_, nm) if nm == "root"));
		assert!(matches!(
			iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		));
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(TokenMetrics { start: 6, end: 11 }, "&".try_into().unwrap())
		);
		assert!(
			matches!(iter.next().unwrap(), Token::ElementFootStart(TokenMetrics{start: 11, end: 17}, nm) if nm == "root")
		);
		assert!(matches!(iter.next().unwrap(), Token::ElementHFEnd(_)));
	}

	#[test]
	fn lexer_lex_decimal_charref() {
		let mut src = &b"<root>&#60;</root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		assert!(matches!(iter.next().unwrap(), Token::ElementHeadStart(_, nm) if nm == "root"));
		assert!(matches!(
			iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		));
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(TokenMetrics { start: 6, end: 11 }, "<".try_into().unwrap())
		);
		assert!(
			matches!(iter.next().unwrap(), Token::ElementFootStart(TokenMetrics{start: 11, end: 17}, nm) if nm == "root")
		);
		assert!(matches!(iter.next().unwrap(), Token::ElementHFEnd(_)));
	}

	#[test]
	fn lexer_lex_hexadecimal_charref() {
		let mut src = &b"<root>&#x3e;</root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		assert!(matches!(iter.next().unwrap(), Token::ElementHeadStart(_, nm) if nm == "root"));
		assert!(matches!(
			iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		));
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(TokenMetrics { start: 6, end: 12 }, ">".try_into().unwrap())
		);
		assert!(
			matches!(iter.next().unwrap(), Token::ElementFootStart(TokenMetrics{start: 12, end: 18}, nm) if nm == "root")
		);
		assert!(matches!(iter.next().unwrap(), Token::ElementHFEnd(_)));
	}

	fn collect_texts<'x, T: Iterator<Item = &'x Token>>(
		iter: &'x mut T,
	) -> (String, usize, usize, Option<&'x Token>) {
		let mut texts: Vec<String> = Vec::new();
		let mut start = 0;
		let mut had_start = false;
		let mut end = 0;
		let mut token: Option<&'x Token> = None;
		for tok in iter {
			match tok {
				Token::Text(metrics, t) => {
					if !had_start {
						start = metrics.start();
						had_start = true;
					} else {
						// text nodes must always be consecutive
						assert_eq!(metrics.start(), end);
					}
					end = metrics.end();
					texts.push(t.to_string());
				}
				other => {
					token = Some(other);
					break;
				}
			}
		}
		let text = texts.join("");
		return (text, start, end, token);
	}

	#[test]
	fn lexer_lex_mixed_text_entities() {
		let mut src =
			&b"<root>&#60;example foo=&quot;bar&quot; baz=&apos;fnord&apos;/&gt;</root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		assert!(matches!(iter.next().unwrap(), Token::ElementHeadStart(_, nm) if nm == "root"));
		assert!(matches!(
			iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		));

		let (text, start, end, _) = collect_texts(&mut iter);

		assert_eq!(start, 6);
		assert_eq!(end, 65);
		assert_eq!(text, "<example foo=\"bar\" baz='fnord'/>");
	}

	#[test]
	fn lexer_lex_reject_charref_with_invalid_cdata() {
		let mut src = &b"<root>&#x00;</root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		match stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink) {
			Err(EndOrError::Error(_)) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn lexer_lex_attribute_amp() {
		let mut src = &b"<root foo='&amp;'>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::Eq(TokenMetrics { start: 9, end: 10 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(TokenMetrics { start: 10, end: 17 }, "&".try_into().unwrap())
		);
	}

	#[test]
	fn lexer_lex_attribute_mixed_with_entities() {
		let mut src =
			&b"<root foo='&#60;example foo=&quot;bar&quot; baz=&apos;fnord&apos;/&gt;'>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::Eq(TokenMetrics { start: 9, end: 10 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 10, end: 71 },
				"<example foo=\"bar\" baz='fnord'/>".try_into().unwrap()
			)
		);
	}

	#[test]
	fn lexer_lex_cdata_section() {
		let mut src = &b"<root><![CDATA[<example foo=\"bar\" baz='fnord'/>]]></root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 6, end: 50 },
				"<example foo=\"bar\" baz='fnord'/>".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementFootStart(
				TokenMetrics { start: 50, end: 56 },
				"root".try_into().unwrap()
			)
		);
		iter.next().unwrap();
	}

	#[test]
	fn lexer_lex_cdata_section_degenerate() {
		let mut src = &b"<root><![CDATA[]]></root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementFootStart(
				TokenMetrics { start: 18, end: 24 },
				"root".try_into().unwrap()
			)
		);
		iter.next().unwrap();
	}

	#[test]
	fn lexer_lex_cdata_section_mixed() {
		let mut src = &b"<root>foobar <![CDATA[Hello <fun>]]</fun>&amp;games world!]]> </root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		);

		let (text, start, end, next) = collect_texts(&mut iter);

		assert_eq!(start, 6);
		assert_eq!(end, 62);
		assert_eq!(text, "foobar Hello <fun>]]</fun>&amp;games world! ");
		assert_eq!(next.unwrap().metrics().start(), 62);
	}

	#[test]
	fn lexer_lex_comment() {
		let mut src = &b"<root>hello<!-- not --> world</root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 6, end: 11 },
				"hello".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::CommentStart(TokenMetrics { start: 11, end: 15 },)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 15, end: 20 },
				" not ".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::CommentEnd(TokenMetrics { start: 20, end: 23 },)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 23, end: 29 },
				" world".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementFootStart(
				TokenMetrics { start: 29, end: 35 },
				"root".try_into().unwrap()
			)
		);
		iter.next().unwrap();
	}

	#[test]
	fn lexer_does_not_expand_references_in_comments() {
		let mut src = &b"<root><!-- &amp; --></root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::CommentStart(TokenMetrics { start: 6, end: 10 },)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 10, end: 17 },
				" &amp; ".try_into().unwrap()
			)
		);
		iter.next().unwrap();
	}

	#[test]
	fn lexer_does_not_emit_empty_text_node_for_empty_comment() {
		let mut src = &b"<root><!----></root>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::CommentStart(TokenMetrics { start: 6, end: 10 },)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::CommentEnd(TokenMetrics { start: 10, end: 13 },)
		);
		iter.next().unwrap();
	}

	#[test]
	fn lexer_lex_restrict_element_name_by_token_length() {
		let mut src = &b"<foobar2342/>"[..];
		let mut lexer = Lexer::new(LexerOptions {
			max_token_length: 6,
		});
		let mut sink = VecSink::new(128);
		match stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink) {
			Err(EndOrError::Error(Error::RestrictedXml(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn lexer_lex_restrict_attribute_name_by_token_length() {
		let mut src = &b"<a foobar2342='foo'/>"[..];
		let mut lexer = Lexer::new(LexerOptions {
			max_token_length: 6,
		});
		let mut sink = VecSink::new(128);
		match stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink) {
			Err(EndOrError::Error(Error::RestrictedXml(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn lexer_lex_restrict_attribute_value_by_token_length() {
		let mut src = &b"<a b='foobar2342'/>"[..];
		let mut lexer = Lexer::new(LexerOptions {
			max_token_length: 6,
		});
		let mut sink = VecSink::new(128);
		match stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink) {
			Err(EndOrError::Error(Error::RestrictedXml(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn lexer_lex_restrict_attribute_value_by_token_length_even_with_entities() {
		let mut src = &b"<a b='foob&amp;rx'/>"[..];
		let mut lexer = Lexer::new(LexerOptions {
			max_token_length: 6,
		});
		let mut sink = VecSink::new(128);
		match stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink) {
			Err(EndOrError::Error(Error::RestrictedXml(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}

	#[test]
	fn lexer_lex_attribute_value_entities_do_only_count_for_expansion() {
		let mut src = &b"<a b='foob&amp;'/>"[..];
		let mut lexer = Lexer::new(LexerOptions {
			max_token_length: 6,
		});
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();
	}

	#[test]
	fn lexer_lex_token_length_causes_text_nodes_to_be_split() {
		let mut src = &b"<a>foo001foo002foo003</a>"[..];
		let mut lexer = Lexer::new(LexerOptions {
			max_token_length: 6,
		});
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 2, end: 3 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 3, end: 9 },
				"foo001".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 9, end: 15 },
				"foo002".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(
				TokenMetrics { start: 15, end: 21 },
				"foo003".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementFootStart(TokenMetrics { start: 21, end: 24 }, "a".try_into().unwrap())
		);
		iter.next().unwrap();
	}

	#[test]
	fn lexer_handles_broken_numeric_entity_correctly() {
		// trimmed testcase, found by afl
		let src = &b"&#;"[..];
		let result = run_fuzz_test(src, 128);
		assert!(result.is_err());
	}

	#[test]
	fn lexer_limits_decimal_entities() {
		let src = &b"&#9999999999;"[..];
		let result = run_fuzz_test(src, 128);
		assert!(result.is_err());
	}

	#[test]
	fn lexer_limits_hexadecimal_entities() {
		let src = &b"&#x9999999999;"[..];
		let result = run_fuzz_test(src, 128);
		assert!(result.is_err());
	}

	#[test]
	fn lexer_rejects_invalid_namestarts() {
		let err = lex_err(b"<123/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedChar(_, '1', None)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<'foo/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedByte(_, b'\'', None)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<.bar/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedChar(_, '.', None)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_invalid_names() {
		let err = lex_err(b"<foo#/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedByte(..)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<f\\a/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedByte(..)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_undeclared_or_invalid_references() {
		let err = lex_err(b"&123;", 128).unwrap();
		match err {
			EndOrError::Error(Error::UndeclaredEntity) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"&foobar;", 128).unwrap();
		match err {
			EndOrError::Error(Error::UndeclaredEntity) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"&?;", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedByte(_, b'?', _)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_non_scalar_char_refs() {
		let err = lex_err(b"&#x110000;", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, true)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_non_xml_10_chars_via_refs_in_text() {
		let err = lex_err(b"&#x00;", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, true)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"&#x1f;", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, true)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_non_xml_10_chars_via_refs_in_attrs() {
		let err = lex_err(b"<a foo='&#x00;'/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, true)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a foo='&#x1f;'/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, true)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_non_xml_10_chars_verbatim_in_text() {
		let err = lex_err(b"\x00", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"\x1f", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_non_xml_10_chars_verbatim_in_attrs() {
		let err = lex_err(b"<a foo='\x00'/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a foo='\x1f'/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, _, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_re_emits_error_on_next_call() {
		let mut src = &b"<a>\x00</a>"[..];
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		let e1 = stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();
		let e2 = stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink)
			.err()
			.unwrap();
		assert_eq!(e1, e2);

		let mut iter = sink.dest.iter();
		assert!(matches!(iter.next().unwrap(), Token::ElementHeadStart(_, nm) if nm == "a"));
		assert!(matches!(iter.next().unwrap(), Token::ElementHFEnd(_)));
		assert!(iter.next().is_none());
	}

	#[test]
	fn lexer_handles_closing_brackets_in_cdata_section() {
		let mut src = &b"<a><![CDATA[]]]></a>"[..];
		let mut lexer = Lexer::new(LexerOptions {
			max_token_length: 6,
		});
		let mut sink = VecSink::new(128);
		stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink).unwrap();

		let mut iter = sink.dest.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 2, end: 3 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(TokenMetrics { start: 3, end: 16 }, "]".try_into().unwrap())
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementFootStart(TokenMetrics { start: 16, end: 19 }, "a".try_into().unwrap())
		);
		iter.next().unwrap();
	}

	#[test]
	fn lexer_recovers_from_wouldblock() {
		let seq = &b"<?xml version='1.0'?>"[..];
		let (sink, result) = lex_chunked(&seq.chunks(5).collect::<Vec<_>>(), 128);
		result.unwrap();

		let mut iter = sink.iter();
		assert_eq!(
			*iter.next().unwrap(),
			Token::XMLDeclStart(TokenMetrics { start: 0, end: 5 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(
				TokenMetrics { start: 6, end: 13 },
				"version".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Eq(TokenMetrics { start: 13, end: 14 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 14, end: 19 },
				"1.0".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::XMLDeclEnd(TokenMetrics { start: 19, end: 21 })
		);
	}

	#[test]
	fn lexer_recovers_from_wouldblock_within_long_whitespace_with_correct_counting() {
		let seq = &b"<?xml   version  =  '1.0'  ?>"[..];
		let (sink, result) = lex_chunked(&seq.chunks(5).collect::<Vec<_>>(), 128);
		result.unwrap();

		let mut iter = sink.iter();
		assert_eq!(
			*iter.next().unwrap(),
			Token::XMLDeclStart(TokenMetrics { start: 0, end: 5 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(
				TokenMetrics { start: 8, end: 15 },
				"version".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Eq(TokenMetrics { start: 17, end: 18 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 20, end: 25 },
				"1.0".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::XMLDeclEnd(TokenMetrics { start: 27, end: 29 })
		);
	}

	#[test]
	fn lexer_rejects_missing_whitespace_between_attrvalue_and_attrname() {
		let err = lex_err(b"<a a='x'b='y'/>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidSyntax(_)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_nonchar_in_comment() {
		let err = lex_err(b"<a><!--\x00--></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 0u32, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a><!---\x00--></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 0u32, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_dashdash_in_comment() {
		let err = lex_err(b"<a><!-- -- --></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidSyntax(_)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_handles_specials_comment_end() {
		let (toks, r) = lex(&b"<root><!----></root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementFootStart(
				TokenMetrics { start: 13, end: 19 },
				"root".try_into().unwrap()
			)
		);
		iter.next().unwrap();

		let (toks, r) = lex(&b"<root><!---->&amp;</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(TokenMetrics { start: 13, end: 18 }, "&".try_into().unwrap())
		);
	}

	#[test]
	fn lexer_rejects_nonchar_after_cr_in_comment() {
		// found with afl
		let err = lex_err(b"<a><!--\r\x01--></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 1, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_allows_cdata_end_marker_in_comment() {
		let (toks, r) = lex(b"<a><!--]]>--></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "]]>");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_allows_comment_marker_in_cdata() {
		let (toks, r) = lex(b"<a><![CDATA[-->]]></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "-->");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_correctly_linefolds_with_comment_marker_in_cdata() {
		let (toks, r) = lex(b"<a><![CDATA[\r-->]]></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n-->");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_folds_crlf_to_lf_in_comment() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a><!--\r\n--></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_cr_folding_in_comment_does_not_break_exit() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a><!--\r--></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_cr_folding_in_comment_does_exit_comment() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a><!--\r<>--></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n<>");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_nonchar_in_cdata_section() {
		let err = lex_err(b"<a><![CDATA[\x00]]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 0u32, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a><![CDATA[]\x00]]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 0u32, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a><![CDATA[]]\x00]]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 0u32, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_cdata_end_in_text() {
		let err = lex_err(b"<a>]]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidSyntax(_)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a>]]]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidSyntax(_)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a>]]]]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidSyntax(_)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_handles_partial_cdata_end() {
		let (toks, r) = lex(&b"<root>]]</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, start, end, next) = collect_texts(&mut iter);
		assert_eq!(text, "]]");
		assert_eq!(start, 6);
		assert_eq!(end, 8);
		assert_eq!(next.unwrap().metrics().start(), 8);

		let (toks, r) = lex(&b"<root>]]foo</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, start, end, next) = collect_texts(&mut iter);
		assert_eq!(text, "]]foo");
		assert_eq!(start, 6);
		assert_eq!(end, 11);
		assert_eq!(next.unwrap().metrics().start(), 11);

		let (toks, r) = lex(&b"<root>]]&gt;</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, start, end, next) = collect_texts(&mut iter);
		assert_eq!(text, "]]>");
		assert_eq!(start, 6);
		assert_eq!(end, 12);
		assert_eq!(next.unwrap().metrics().start(), 12);

		let (toks, r) = lex(&b"<root>]]]</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, start, end, next) = collect_texts(&mut iter);
		assert_eq!(text, "]]]");
		assert_eq!(start, 6);
		assert_eq!(end, 9);
		assert_eq!(next.unwrap().metrics().start(), 9);

		let (toks, r) = lex(&b"<root>]]]foo</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, start, end, next) = collect_texts(&mut iter);
		assert_eq!(text, "]]]foo");
		assert_eq!(start, 6);
		assert_eq!(end, 12);
		assert_eq!(next.unwrap().metrics().start(), 12);

		let (toks, r) = lex(&b"<root>]]]&gt;</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, start, end, next) = collect_texts(&mut iter);
		assert_eq!(text, "]]]>");
		assert_eq!(start, 6);
		assert_eq!(end, 13);
		assert_eq!(next.unwrap().metrics().start(), 13);
	}

	#[test]
	fn lexer_handles_specials_after_cdata_end() {
		let (toks, r) = lex(&b"<root><![CDATA[]]></root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementHFEnd(TokenMetrics { start: 5, end: 6 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::ElementFootStart(
				TokenMetrics { start: 18, end: 24 },
				"root".try_into().unwrap()
			)
		);
		iter.next().unwrap();

		let (toks, r) = lex(&b"<root><![CDATA[]]>&amp;</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		assert_eq!(
			*iter.next().unwrap(),
			Token::Text(TokenMetrics { start: 18, end: 23 }, "&".try_into().unwrap())
		);

		let (toks, r) = lex(&b"<root><![CDATA[]]><![CDATA[]]]]>&gt;</root>"[..], 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, start, end, next) = collect_texts(&mut iter);
		assert_eq!(text, "]]>");
		assert_eq!(start, 18);
		assert_eq!(end, 36);
		assert_eq!(next.unwrap().metrics().start(), 36);
	}

	#[test]
	fn lexer_rejects_nonchar_in_cdata_end_in_text() {
		let err = lex_err(b"<a>]\x00]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 0u32, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}

		let err = lex_err(b"<a>]]\x00></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 0u32, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_numeric_start_of_name_in_opening_tag() {
		// found via fuzzing by moparisthebest
		let err = lex_err(b"<4foo>", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedChar(_, '4', None)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_numeric_start_of_name_in_closing_tag() {
		// found via fuzzing by moparisthebest
		let err = lex_err(b"</4foo>", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedChar(_, '4', None)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_zero_length_name_in_opening_tag() {
		// found via fuzzing by moparisthebest
		let err = lex_err(b"< >", 128).unwrap();
		match err {
			EndOrError::Error(Error::UnexpectedByte(_, b' ', None)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_zero_length_name_in_closing_tag() {
		// found via fuzzing by moparisthebest
		let err = lex_err(b"</ >", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidSyntax(_)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_lex_accounts_whitespace_between_xml_decl_and_element_to_element() {
		// even though this behaviour may seem strange, it is useful for making sure that all bytes are accounted for in the parser.
		// discarding whitespace within an element is fine because that is a single event in the parser and it can reconstruct the gaps from the token metrics
		let mut src = "<?xml version=\"1.0\" encoding='utf-8'?>\n\n<root/>".as_bytes();
		let mut lexer = new_lexer();
		let mut sink = VecSink::new(128);
		let result = stream_to_sink_from_bytes(&mut lexer, &mut src, &mut sink);

		assert!(result.is_ok());

		let mut iter = sink.dest.iter();
		assert_eq!(
			*iter.next().unwrap(),
			Token::XMLDeclStart(TokenMetrics { start: 0, end: 5 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(
				TokenMetrics { start: 6, end: 13 },
				"version".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Eq(TokenMetrics { start: 13, end: 14 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 14, end: 19 },
				"1.0".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Name(
				TokenMetrics { start: 20, end: 28 },
				"encoding".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::Eq(TokenMetrics { start: 28, end: 29 })
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::AttributeValue(
				TokenMetrics { start: 29, end: 36 },
				"utf-8".try_into().unwrap()
			)
		);
		assert_eq!(
			*iter.next().unwrap(),
			Token::XMLDeclEnd(TokenMetrics { start: 36, end: 38 })
		);
		match iter.next().unwrap() {
			Token::ElementHeadStart(tm, ..) => {
				assert_eq!(*tm, TokenMetrics { start: 38, end: 45 });
			}
			other => panic!("unexpected event: {:?}", other),
		}
	}

	#[test]
	fn lexer_folds_crlf_to_lf_in_text() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a>\r\n</a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_nonchar_after_cr() {
		let err = lex_err(b"<a>\r\x01</a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 1, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_rejects_nonchar_after_cr_in_cdata() {
		// found with afl
		let err = lex_err(b"<a><![CDATA[\r\x01]]></a>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidChar(_, 1, false)) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_does_not_modify_charrefs_for_line_endings() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a>&#xd;&#xa;</a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		let (text, _, _, _) = collect_texts(&mut iter);
		assert_eq!(text, "\r\n");
	}

	#[test]
	fn lexer_folds_crlf_to_lf_in_cdata() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a><![CDATA[\r\n]]></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_cr_folding_does_not_break_specials() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a>\r</a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_cr_folding_in_cdata_does_not_break_exit() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a><![CDATA[\r]]></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_cr_folding_in_cdata_does_exit_cdata_section() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a><![CDATA[\r<>]]></a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n<>");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_folds_crcrlf_to_lflf_in_text() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a>\r\r\n</a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_folds_cr_to_lf_in_text() {
		// XML 1.0 § 2.11
		let (toks, r) = lex(b"<a>\r</a>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::Text(_, cdata) => {
				assert_eq!(cdata, "\n");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_normalizes_whitespace_in_attributes() {
		// XML 1.0 § 3.3.3
		let (toks, r) = lex(b"<a x='\r\r\n\t '/>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::AttributeValue(_, cdata) => {
				// just four spaces, because CRLF normalization happens before attribute value normalization
				// gotta love this
				assert_eq!(cdata, "    ");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_handles_crlf_in_attribute() {
		// XML 1.0 § 3.3.3
		let (toks, r) = lex(b"<a x='\r\n\t '/>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::AttributeValue(_, cdata) => {
				assert_eq!(cdata, "   ");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_preserves_whitespace_inserted_via_charrefs_in_attributes() {
		// XML 1.0 § 3.3.3
		let (toks, r) = lex(b"<a x='&#xd;&#xa;&#x9; '/>", 128);
		r.unwrap();

		let mut iter = toks.iter();
		iter.next().unwrap();
		iter.next().unwrap();
		iter.next().unwrap();
		match iter.next().unwrap() {
			Token::AttributeValue(_, cdata) => {
				assert_eq!(cdata, "\r\n\t ");
			}
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn lexer_is_resilient_to_chunking() {
		let (_toks, r) = lex_chunked(&[&b"<foo bar='baz' "[..], &b"fnord=''/>"[..]], 128);
		r.unwrap();
	}

	#[test]
	fn lexer_emits_close_tag_token_even_at_end_of_buffer() {
		let mut buf = &b"</foo>"[..];
		let mut lexer = new_lexer();
		match lexer.lex_bytes(&mut buf, false) {
			Ok(Some(Token::ElementFootStart(_, _))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match lexer.lex_bytes(&mut buf, false) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match lexer.lex_bytes(&mut buf, false) {
			Err(EndOrError::NeedMoreData) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}

	#[test]
	fn lexer_catches_broken_utf8_sequence_at_end_of_file() {
		let mut buf = &b"<xyz>\xf0\x9f\x8e"[..];
		let mut lexer = new_lexer();
		match lexer.lex_bytes(&mut buf, true) {
			Ok(Some(Token::ElementHeadStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match lexer.lex_bytes(&mut buf, true) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match lexer.lex_bytes(&mut buf, true) {
			Err(EndOrError::Error(Error::InvalidUtf8Byte(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}

	#[test]
	fn lexer_catches_incorrect_utf8_at_end_of_file() {
		let mut buf = &b"<xyz>\xf0\x9f\x8e\xff"[..];
		let mut lexer = new_lexer();
		match lexer.lex_bytes(&mut buf, true) {
			Ok(Some(Token::ElementHeadStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match lexer.lex_bytes(&mut buf, true) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match lexer.lex_bytes(&mut buf, true) {
			Err(EndOrError::Error(Error::InvalidUtf8Byte(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}

	#[test]
	fn lexer_catches_split_utf8_sequence_at_cdata_section_boundary() {
		let err = lex_err(b"<xyz>\xe2<![CDATA[\x98\xba]]></xyz>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidUtf8Byte(0xe2)) => (),
			other => panic!("unexpected error: {other:?}"),
		}

		let err = lex_err(b"<xyz><![CDATA[\xe2\x98]]>\xba</xyz>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidUtf8Byte(0xe2)) => (),
			other => panic!("unexpected error: {other:?}"),
		}
	}

	#[test]
	fn lexer_catches_split_utf8_sequence_at_comment_boundary() {
		let err = lex_err(b"<xyz>\xe2<!--\x98\xba--></xyz>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidUtf8Byte(0xe2)) => (),
			other => panic!("unexpected error: {other:?}"),
		}

		let err = lex_err(b"<xyz><!--\xe2\x98-->\xba</xyz>", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidUtf8Byte(0xe2)) => (),
			other => panic!("unexpected error: {other:?}"),
		}
	}

	fn feed_bytewise(lexer: &mut Lexer, src: &mut &[u8]) -> Result<Option<Token>> {
		loop {
			let ((mut buf, remainder), at_eof) = if !src.is_empty() {
				(src.split_at(1), src.len() == 1)
			} else {
				((&[][..], *src), true)
			};
			let result = lexer.lex_bytes(&mut buf, at_eof);
			if buf.is_empty() {
				*src = remainder;
			}
			match result {
				Ok(v) => return Ok(v),
				Err(EndOrError::NeedMoreData) => {
					continue;
				}
				Err(other) => return Err(other),
			};
		}
	}

	#[test]
	fn lexer_handles_chunked_utf8_fed_bytewise() {
		let mut src = "<xyz>fööbär🎉</xyz>".as_bytes();
		let mut lexer = new_lexer();
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHeadStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "fööbär🎉");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementFootStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}

	#[test]
	fn lexer_detect_eof_in_name() {
		let err = lex_err(b"<aa", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidEof(Some(ErrorContext::Name))) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_in_element_head_whitespace() {
		let err = lex_err(b"<aa  ", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidEof(Some(ErrorContext::Element))) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_in_attrname() {
		let err = lex_err(b"<a xxxx", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidEof(Some(ErrorContext::Name))) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_after_attrname() {
		let err = lex_err(b"<a x=", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidEof(Some(ErrorContext::Element))) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_in_attrval() {
		let err = lex_err(b"<a x='xyz", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidEof(Some(ErrorContext::AttributeValue))) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_after_attr() {
		let err = lex_err(b"<a x=''", 128).unwrap();
		match err {
			EndOrError::Error(Error::InvalidEof(Some(ErrorContext::Element))) => (),
			other => panic!("unexpected error: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_after_head() {
		let (_, result) = lex(b"<a>", 128);
		// this is a proper Eof, the parser will have to decide whether it's ok or not here'
		match result {
			Ok(()) => (),
			other => panic!("unexpected lex result: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_in_text() {
		let (_, result) = lex(b"<a>foo", 128);
		// this is a proper Eof, the parser will have to decide whether it's ok or not here'
		match result {
			Ok(()) => (),
			other => panic!("unexpected lex result: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_at_end_of_document() {
		let (_, result) = lex(b"<a/>", 128);
		match result {
			Ok(()) => (),
			other => panic!("unexpected lex result: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_at_end_of_document_with_whitespace() {
		let (_, result) = lex(b"<a/>\n\n", 128);
		match result {
			Ok(()) => (),
			other => panic!("unexpected lex result: {:?}", other),
		}
	}

	#[test]
	fn lexer_detect_eof_before_decl_or_el() {
		let (_, result) = lex(b"", 128);
		// this is a proper Eof, the parser will have to decide whether it's ok or not here'
		match result {
			Ok(()) => (),
			other => panic!("unexpected lex result: {:?}", other),
		}
	}

	#[test]
	fn utf8_sequence_on_token_boundary() {
		let inner = "あああ";
		let buf = format!("<x>{}</x>", inner);
		// NOTE: lex limit < 3 won't work at all because that's shorter than
		// the valid UTF-8 sequence.
		for limit in 3..(inner.len() + 1) {
			let mut lexer = Lexer::new(LexerOptions {
				max_token_length: limit,
			});
			let mut sink = VecSink::new(128);
			let result = stream_to_sink(&mut lexer, &mut buf.as_bytes(), &mut sink, true);
			// this is a proper Eof, the parser will have to decide whether it's ok or not here'
			match result {
				Ok(()) => (),
				other => panic!("unexpected lex result (limit={}): {:?}", limit, other),
			}
		}
	}

	#[test]
	fn unbuffered_text_fed_chunkwise() {
		let mut lexer = new_lexer();
		lexer.text_buffering = false;

		let (tokens, result) = lex_chunked_with(
			&[
				&b"<xyz>f\xc3"[..],
				&b"\xb6\xc3\xb6"[..],
				"bär🎉</xyz>".as_bytes(),
			],
			128,
			lexer,
		);
		result.unwrap();
		let mut tokens = tokens.into_iter();
		match tokens.next() {
			Some(Token::ElementHeadStart(_, name)) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected token: {:?}", other),
		}
		match tokens.next() {
			Some(Token::ElementHFEnd(_)) => (),
			other => panic!("unexpected token: {:?}", other),
		}
		match tokens.next() {
			Some(Token::Text(_, text)) => {
				assert_eq!(text, "f");
			}
			other => panic!("unexpected token: {:?}", other),
		}
		match tokens.next() {
			Some(Token::Text(_, text)) => {
				assert_eq!(text, "öö");
			}
			other => panic!("unexpected token: {:?}", other),
		}
		match tokens.next() {
			Some(Token::Text(_, text)) => {
				assert_eq!(text, "bär🎉");
			}
			other => panic!("unexpected token: {:?}", other),
		}
		match tokens.next() {
			Some(Token::ElementFootStart(_, name)) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected token: {:?}", other),
		}
		match tokens.next() {
			Some(Token::ElementHFEnd(_)) => (),
			other => panic!("unexpected token: {:?}", other),
		}
		match tokens.next() {
			None => (),
			other => panic!("unexpected token: {:?}", other),
		}
	}

	#[test]
	fn unbuffered_text_fed_bytewise() {
		let mut src = "<xyz>fööbär🎉</xyz>".as_bytes();
		let mut lexer = new_lexer();
		lexer.text_buffering = false;
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHeadStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "f");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "ö");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "ö");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "b");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "ä");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "r");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::Text(_, text))) => {
				assert_eq!(text, "🎉");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementFootStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}

	#[test]
	fn unbuffered_text_detects_and_reports_truncated_utf8() {
		let mut src = &b"<xyz>\xf0\x9f\x8emore</xyz>"[..];
		let mut lexer = new_lexer();
		lexer.text_buffering = false;
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHeadStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Err(EndOrError::Error(Error::InvalidUtf8Byte(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}

	#[test]
	fn unbuffered_text_detects_and_reports_invalid_utf8() {
		let mut src = &b"<xyz>\xf0\x9f\xff\x89more</xyz>"[..];
		let mut lexer = new_lexer();
		lexer.text_buffering = false;
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHeadStart(_, name))) => {
				assert_eq!(name, "xyz");
			}
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Ok(Some(Token::ElementHFEnd(_))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
		match feed_bytewise(&mut lexer, &mut src) {
			Err(EndOrError::Error(Error::InvalidUtf8Byte(..))) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}
}
