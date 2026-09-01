use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::io;

use tokio::io::AsyncBufRead;

#[cfg(feature = "stream")]
use futures_core::stream::Stream;

use crate::error::EndOrError;
use crate::parser::{Options, Parse, Parser, RawParser, WithOptions};
use crate::Error;

use pin_project_lite::pin_project;

/// Future implementing [`GenericAsyncReader::read`].
pub struct ReadEvent<'x, T, P: Parse> {
	inner: Pin<&'x mut GenericAsyncReader<T, P>>,
}

impl<T: AsyncBufRead + Unpin, P: Parse> Future for ReadEvent<'_, T, P> {
	type Output = io::Result<Option<P::Output>>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		self.inner.as_mut().poll_read(cx)
	}
}

/// Future implementing [`GenericAsyncReader::read_all`].
pub struct ReadAll<'x, T, P: Parse, F> {
	cb: F,
	inner: Pin<&'x mut GenericAsyncReader<T, P>>,
}

impl<P: Parse, T: AsyncBufRead + Unpin, F: FnMut(P::Output) + Send + Unpin> Future
	for ReadAll<'_, T, P, F>
{
	type Output = io::Result<()>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		loop {
			match self.inner.as_mut().poll_read(cx) {
				Poll::Ready(Ok(Some(ev))) => {
					(self.cb)(ev);
				}
				Poll::Ready(Ok(None)) => return Poll::Ready(Ok(())),
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}

#[cfg(feature = "stream")]
#[cfg_attr(docsrs, doc(cfg(all(feature = "stream", feature = "tokio"))))]
impl<T: AsyncBufRead, P: Parse> Stream for GenericAsyncReader<T, P> {
	type Item = io::Result<P::Output>;

	fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		match self.poll_read(cx) {
			Poll::Pending => Poll::Pending,
			Poll::Ready(Ok(Some(v))) => Poll::Ready(Some(Ok(v))),
			Poll::Ready(Ok(None)) => Poll::Ready(None),
			Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
		}
	}
}

pin_project! {
	/**
	Generic tokio-compatible asynchronous driver for restricted XML parsers.

	This type is best used through its aliases:

	- [`AsyncReader`] which uses [`Parser`] and provides full XML namespacing
	  support
	- [`AsyncRawReader`] which uses [`RawParser`] and comes with limitations
	  around validity checking and does not support XML namespaces.

	The aliases have more extensive usage documentation as well as examples.
	*/
	#[project = AsyncReaderProj]
	pub struct GenericAsyncReader<T, P: Parse = Parser>{
		#[pin]
		inner: T,
		parser: P,
	}
}

fn parse_step<P: Parse>(
	parser: &mut P,
	buf: &mut &[u8],
	may_eof: bool,
) -> (usize, Poll<Result<Option<P::Output>, Error>>) {
	let old_len = buf.len();
	// need to guard eof with the buf len here, because we only know that we are actually at eof by the fact that we see an empty buffer.
	let at_eof = may_eof && buf.is_empty();
	let result = parser.parse(buf, at_eof);
	let new_len = buf.len();
	assert!(new_len <= old_len);
	let read = old_len - new_len;
	match result {
		Ok(v) => (read, Poll::Ready(Ok(v))),
		Err(EndOrError::NeedMoreData) => (read, Poll::Pending),
		Err(EndOrError::Error(e)) => (read, Poll::Ready(Err(e))),
	}
}

/// Plumbing function to asynchronously parse data.
///
/// This is the backing implementation of [`GenericAsyncReader::poll_read`]
/// and in many cases, using a [`GenericAsyncReader`] (or the convenient type
/// alias, [`AsyncReader`]) is preferable.
///
/// However, if the parser is being used as part of a full-duplex operation on
/// the underlying I/O object, it can be useful to not have the parser take
/// ownership over the I/O object.
pub fn poll_parse_from<P: Parse, Reader: AsyncBufRead>(
	mut r: Pin<&mut Reader>,
	parser: &mut P,
	cx: &mut Context<'_>,
) -> Poll<io::Result<Option<P::Output>>> {
	loop {
		let mut buf = match r.as_mut().poll_fill_buf(cx) {
			Poll::Pending => {
				// a.k.a. WouldBlock
				// we always try an empty read here because the lexer needs that
				return parse_step(parser, &mut &[][..], false)
					.1
					.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
			}
			Poll::Ready(Ok(buf)) => buf,
			Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
		};
		let (consumed, result) = parse_step(parser, &mut buf, true);
		r.as_mut().consume(consumed);
		match result {
			// if we get a pending here, we need to ask the source for more data!
			Poll::Pending => continue,
			Poll::Ready(v) => {
				return Poll::Ready(v.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)));
			}
		}
	}
}

impl<T, P: Parse + Default> GenericAsyncReader<T, P> {
	/// Create a reader using a parser with default options, wrapping the
	/// given reader.
	pub fn new(inner: T) -> Self {
		Self::wrap(inner, P::default())
	}
}

impl<T, P: Parse + WithOptions> GenericAsyncReader<T, P> {
	/// Create a reader while configuring the parser with the given options.
	pub fn with_options(inner: T, options: Options) -> Self {
		Self::wrap(inner, P::with_options(options))
	}
}

impl<T, P: Parse> GenericAsyncReader<T, P> {
	/// Create a reader from its inner parts.
	pub fn wrap(inner: T, parser: P) -> Self {
		Self { inner, parser }
	}

	/// Decompose the AsyncReader into its parts
	pub fn into_inner(self) -> (T, P) {
		(self.inner, self.parser)
	}

	/// Access the inner AsyncBufRead
	pub fn inner(&self) -> &T {
		&self.inner
	}

	/// Access the inner AsyncBufRead, mutably
	pub fn inner_mut(&mut self) -> &mut T {
		&mut self.inner
	}

	/// Access the inner AsyncBufRead, mutably and pinned.
	pub fn inner_pinned(self: Pin<&mut Self>) -> Pin<&mut T> {
		let this = self.project();
		this.inner
	}

	/// Access the parser
	pub fn parser(&self) -> &P {
		&self.parser
	}

	/// Access the parser, mutably
	pub fn parser_mut(&mut self) -> &mut P {
		&mut self.parser
	}

	/// Access the inner parser, mutably and while pinned.
	pub fn parser_pinned(self: Pin<&mut Self>) -> &mut P {
		let this = self.project();
		this.parser
	}

	/// Release temporary buffers and other ephemeral allocations.
	///
	/// This is sensible to call when it is expected that no more data will be
	/// processed by the parser for a while and the memory is better used
	/// elsewhere.
	#[inline(always)]
	#[doc(hidden)]
	#[deprecated(
		since = "0.12.0",
		note = "use .parser_mut().release_temporaries() / .parser_pinned().release_temporaries()"
	)]
	pub fn release_temporaries(&mut self) {
		self.parser.release_temporaries();
	}
}

impl<T: AsyncBufRead, P: Parse> GenericAsyncReader<T, P> {
	/// Attempts to parse a single event from the source.
	///
	/// If the EOF has been reached with a valid document, `None` is returned.
	///
	/// I/O errors may be retried, all other errors are fatal (and will be
	/// returned again by the parser on the next invocation without reading
	/// further data from the source).
	///
	/// In most cases, it is advisable to use [`read`][`Self::read`] instead.
	pub fn poll_read(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
	) -> Poll<io::Result<Option<P::Output>>> {
		let this = self.project();
		poll_parse_from(this.inner, this.parser, cx)
	}
}

impl<T: AsyncBufRead + Unpin, P: Parse> GenericAsyncReader<T, P> {
	/// Read a single event from the parser.
	///
	/// # End-of-file handling
	///
	/// If `poll_fill_buf()` returns an empty buffer, it is treated as the end
	/// of file. At end of file, either the return value `None` is produced or
	/// an error.
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
	/// Equivalent to:
	///
	/// ```ignore
	/// async fn read(&mut self) -> Result<Option<Event>>;
	/// ```
	pub fn read(&mut self) -> ReadEvent<'_, T, P> {
		ReadEvent {
			inner: Pin::new(self),
		}
	}

	/// Read all events which can be produced from the data source (at this
	/// point in time).
	///
	/// The given `cb` is invoked for each event.
	///
	/// # End-of-file handling
	///
	/// If `poll_fill_buf()` returns an empty buffer, it is treated as the end
	/// of file. At end of file the function returns (either successfully or
	/// with an error).
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
	/// Equivalent to:
	///
	/// ```ignore
	///     async fn read_all<F>(&mut self, mut cb: F) -> Result<()>
	///            where F: FnMut(Event) -> () + Send
	/// ```
	pub fn read_all<F>(&mut self, cb: F) -> ReadAll<'_, T, P, F> {
		ReadAll {
			inner: Pin::new(self),
			cb,
		}
	}
}

/**
# Tokio-compatible asynchronous restricted XML 1.0 parser

The [`AsyncReader`] allows parsing XML documents from a
[`tokio::io::AsyncBufRead`], asynchronously. It operates similarly as the
[`Reader`][`crate::Reader`] does, but it works with asynchronous data
sources instead of synchronous (blocking) ones.

As it bases on [`Parser`] (instead of [`RawParser`]), namespace prefixes are
resolved by the parser and attributes are collected in a map before they are
handed to the application. If you do not need XML namespace support and can
tolerate the caveats of the [`RawParser`] (see its documentation for details),
[`AsyncRawReader`] may be a more suitable type for you.

[`Event`][`crate::Event`]s can be obtained through [`read`][`Self::read`] and
[`read_all`][`Self::read_all`]. If the `stream` feature is enabled,
[`AsyncReader`] also implements the
[`Stream`][`futures_core::stream::Stream`] trait.

## Example

The example is a bit pointless because it does not really demonstrate the
asynchronicity.

```
use rxml::{AsyncReader, Error, Event, XmlVersion};
use tokio::io::AsyncRead;
# tokio_test::block_on(async {
let mut doc = &b"<?xml version='1.0'?><hello>World!</hello>"[..];
// this converts the doc into an tokio::io::AsyncRead
let mut pp = AsyncReader::new(&mut doc);
// we expect the first event to be the XML declaration
let ev = pp.read().await;
assert!(matches!(ev, Ok(Some(Event::XmlDeclaration(_, XmlVersion::V1_0)))));
# })
```
*/
pub type AsyncReader<T> = GenericAsyncReader<T, Parser>;

/**
# Low-level tokio-compatible asynchronous restricted XML 1.0 parser (without namespace support)

The [`AsyncRawReader`] allows parsing XML documents from a
[`tokio::io::AsyncBufRead`], asynchronously. It operates similarly as the
[`Reader`][`crate::Reader`] does, but it works with asynchronous data
sources instead of synchronous (blocking) ones.

As it bases on [`RawParser`] (instead of [`Parser`]), namespace prefixes are
not resolved by the parser and need to be resolved by the application. For
further caveats, please see the [`RawParser`] documentation. If you need proper
XML namespace support, consider using [`AsyncReader`] instead.

[`RawEvent`][`crate::RawEvent`]s can be obtained through
[`read`][`Self::read`] and [`read_all`][`Self::read_all`]. If the `stream`
feature is enabled, [`AsyncRawReader`] also implements the
[`Stream`][`futures_core::stream::Stream`] trait.

## Example

The example is a bit pointless because it does not really demonstrate the
asynchronicity.

```
use rxml::{AsyncRawReader, Error, RawEvent, RawParser, XmlVersion};
use tokio::io::AsyncRead;
# tokio_test::block_on(async {
let mut doc = &b"<?xml version='1.0'?><hello>World!</hello>"[..];
// this converts the doc into an tokio::io::AsyncRead
let mut pp = AsyncRawReader::new(&mut doc);
// we expect the first event to be the XML declaration
let ev = pp.read().await;
assert!(matches!(ev.unwrap().unwrap(), RawEvent::XmlDeclaration(_, XmlVersion::V1_0)));
# })
```
*/
pub type AsyncRawReader<T> = GenericAsyncReader<T, RawParser>;
