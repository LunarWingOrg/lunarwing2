/*!
Wrappers around lexers and parsers to drive them.

For high-level parsing, [`Reader`] is the thing to look at. More information
and examples can also be found in the [`rxml`] top-level documentation.

   [`rxml`]: crate
*/

use std::io;

use crate::error::EndOrError;
use crate::parser::{Options, Parse, Parser, RawParser, WithOptions};

/// Convert end-of-file-ness of a result to a boolean flag.
///
/// If the result is ok, return true (EOF). If the result is not ok, but the
/// error is an I/O error indicating that the data source would have to block
/// to read further data, return false ("Ok, but not at eof yet").
///
/// All other errors are passed through.
pub fn as_eof_flag(r: io::Result<()>) -> io::Result<bool> {
	match r {
		Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
		Err(other) => Err(other),
		Ok(()) => Ok(true),
	}
}

/**
Generic driver for restricted XML parsers.

This type is best used through its aliases:

- [`Reader`] which uses [`Parser`] and provides full XML namespacing support
- [`RawReader`] which uses [`RawParser`] and comes with limitations
	around validity checking and does not support XML namespaces.

The aliases have more extensive usage documentation as well as examples.
*/
#[derive(Debug)]
pub struct GenericReader<T: io::BufRead, P: Parse> {
	parser: P,
	reader: T,
}

impl<T: io::BufRead, P: Parse + Default> GenericReader<T, P> {
	/// Create a reader using a parser with default options, wrapping the
	/// given reader.
	pub fn new(inner: T) -> Self {
		Self::wrap(inner, P::default())
	}
}

impl<T: io::BufRead, P: Parse + WithOptions> GenericReader<T, P> {
	/// Create a reader while configuring the parser with the given options.
	pub fn with_options(inner: T, options: Options) -> Self {
		Self::wrap(inner, P::with_options(options))
	}
}

impl<T: io::BufRead, P: Parse> GenericReader<T, P> {
	/// Create a reader from its inner parts.
	pub fn wrap(inner: T, parser: P) -> Self {
		Self {
			reader: inner,
			parser,
		}
	}

	/// Decompose the reader into its backing reader and the parser.
	pub fn into_inner(self) -> (T, P) {
		(self.reader, self.parser)
	}

	/// Access the inner BufRead
	pub fn inner(&self) -> &T {
		&self.reader
	}

	/// Access the inner BufRead, mutably
	pub fn inner_mut(&mut self) -> &mut T {
		&mut self.reader
	}

	/// Access the parser
	pub fn parser(&self) -> &P {
		&self.parser
	}

	/// Access the parser, mutably
	pub fn parser_mut(&mut self) -> &mut P {
		&mut self.parser
	}

	/// Read a single event from the source.
	///
	/// This function will issue zero or more calls to `fill_buf()` in order
	/// to parse data.
	///
	/// # End-of-file handling
	///
	/// If `fill_buf()` returns an empty buffer, it is treated as the end of
	/// file. At end of file, either the return value `None` is produced or an
	/// error.
	///
	/// # I/O error handling
	///
	/// Any I/O error is passed back to the caller. This allows any I/O error
	/// to be retried (though the success of that will obviously depend on the
	/// backing reader).
	///
	/// # Parser error handling
	///
	/// Errors returned by the parser are fatal and are returned as
	/// [`InvalidData`][`std::io::ErrorKind::InvalidData`]
	/// [`io::Error`][`std::io::Error`] error values.
	///
	/// # Blocking I/O
	///
	/// If the [`Reader`] is used with blocking I/O and a source which may
	/// block for a significant amount of time (e.g. a network socket), some
	/// events may be emitted with significant delay. This is due to an edge
	/// case where the lexer may emit a token without consuming a byte from
	/// the source.
	///
	/// This internal state of the lexer is not observable from the outside,
	/// but it affects most importantly closing element tags. In practice,
	/// this means that the last closing element tag of a "stanza" of XML is
	/// only going to be emitted once the first byte of the next stanza has
	/// been made available through the BufRead.
	///
	/// This only affects blocking I/O, because a non-blocking source will
	/// return [`std::io::ErrorKind::WouldBlock`] from the read call and
	/// yield control back to the parser to emit the event.
	///
	/// In general, for networked operations, it is recommended to use
	/// [`AsyncReader`][`crate::AsyncReader`] instead, or use the
	/// [`Parser`]/[`RawParser`][`crate::RawParser`] directly.
	///
	/// # Return value
	///
	/// Returns `None` if a valid end of file is reached, a token if a valid
	/// token is encountered or an error otherwise.
	pub fn read(&mut self) -> io::Result<Option<P::Output>> {
		let mut empty: &[u8] = &[][..];
		// Important: We must try an empty read before asking the reader for
		// more data! If we don't do this, we may be missing tokens or seeing
		// them late, which may cause applications to get stuck!
		match self.parser.parse(&mut empty, false) {
			Ok(v) => return Ok(v),
			// need more data, continue
			Err(EndOrError::NeedMoreData) => (),
			Err(EndOrError::Error(other)) => {
				return Err(io::Error::new(io::ErrorKind::InvalidData, other));
			}
		};
		loop {
			let mut buf = self.reader.fill_buf()?;
			let init_len = buf.len();
			let result = self.parser.parse(&mut buf, init_len == 0);
			let new_len = buf.len();
			let consumed = init_len - new_len;
			self.reader.consume(consumed);
			match result {
				Ok(v) => return Ok(v),
				Err(EndOrError::NeedMoreData) => {
					assert!(consumed == init_len);
				}
				Err(EndOrError::Error(other)) => {
					return Err(io::Error::new(io::ErrorKind::InvalidData, other));
				}
			}
		}
	}

	/// Read all events which can be produced from the data source.
	///
	/// The given `callback` is invoked for each event.
	///
	/// I/O errors may be retried, all other errors are fatal (and will be
	/// returned again by the parser on the next invocation without reading
	/// further data from the source).
	pub fn read_all<F>(&mut self, mut callback: F) -> io::Result<()>
	where
		F: FnMut(P::Output),
	{
		loop {
			match self.read()? {
				None => return Ok(()),
				Some(ev) => callback(ev),
			}
		}
	}

	/// Wrapper around [`as_eof_flag`][`crate::as_eof_flag`] and [`read_all`][`Self::read_all`].
	#[inline(always)]
	pub fn read_all_eof<F>(&mut self, callback: F) -> io::Result<bool>
	where
		F: FnMut(P::Output),
	{
		crate::as_eof_flag(self.read_all(callback))
	}

	/// Release all temporary buffers or other ephemeral allocations
	///
	/// This is sensible to call when it is expected that no more data will be
	/// processed by the parser for a while and the memory is better used
	/// elsewhere.
	pub fn release_temporaries(&mut self) {
		self.parser.release_temporaries();
	}
}

impl<T: io::BufRead, P: Parse> Iterator for GenericReader<T, P> {
	type Item = io::Result<P::Output>;

	fn next(&mut self) -> Option<Self::Item> {
		self.read().transpose()
	}
}

/**
# Restricted XML 1.0 parser

This reader allows to read XML events from a [`std::io::BufRead`]. A `BufRead`
is required for performance reasons.

As it bases on [`Parser`] (instead of [`RawParser`]), namespace prefixes are
resolved by the parser and attributes are collected in a map before they are
handed to the application. If you do not need XML namespace support and can
tolerate the caveats of the [`RawParser`] (see its documentation for details),
[`RawReader`] may be a more suitable type for you.

[`Event`][`crate::Event`]s can be obtained through [`read`][`Self::read`] and
[`read_all`][`Self::read_all`]. The [`Reader`] also implements the
[`Iterator`] trait.

## Example

```
use rxml::{Reader, Error, Event, XmlVersion};
use std::io;
use std::io::BufRead;
let mut doc = &b"<?xml version='1.0'?><hello>World!</hello>"[..];
// this converts the doc into an io::BufRead
let mut pp = Reader::new(&mut doc);
// we expect the first event to be the XML declaration
let ev = pp.read();
assert!(matches!(ev.unwrap().unwrap(), Event::XmlDeclaration(_, XmlVersion::V1_0)));
```
*/
pub type Reader<T> = GenericReader<T, Parser>;

/**
# Low-level restricted XML 1.0 parser (without namespace support)

This reader allows to read XML events from a [`std::io::BufRead`]. A `BufRead`
is required for performance reasons.

As it bases on [`RawParser`] (instead of [`Parser`]), namespace prefixes are
not resolved by the parser and need to be resolved by the application. For
further caveats, please see the [`RawParser`] documentation. If you need proper
XML namespace support, consider using [`Reader`] instead.

[`RawEvent`][`crate::Event`]s can be obtained through [`read`][`Self::read`]
and [`read_all`][`Self::read_all`]. The [`Reader`] also implements the
[`Iterator`] trait.

## Example

```
use rxml::{RawReader, Error, RawEvent, XmlVersion};
use std::io;
use std::io::BufRead;
let mut doc = &b"<?xml version='1.0'?><hello>World!</hello>"[..];
// this converts the doc into an io::BufRead
let mut pp = RawReader::new(&mut doc);
// we expect the first event to be the XML declaration
let ev = pp.read();
assert!(matches!(ev.unwrap().unwrap(), RawEvent::XmlDeclaration(_, XmlVersion::V1_0)));
```
*/
pub type RawReader<T> = GenericReader<T, RawParser>;
