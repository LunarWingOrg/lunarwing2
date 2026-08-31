/*!
# Restricted XML 1.0 parsing facilities

This module contains parsing facilities for XML documents. To satisfy
different styles of tree building, the parsing step is separated in two
stages: the logical stage and the namespace/attribute resolution stage.

## Logical stage

In the logical stage, the logical elements of the XML document are emitted as
[`RawEvent`] structs. These *may* be used by end-users to build XML document
models, but they are not completely validated even to the XML 1.0
specification. The caveats are documented at the [`RawParser`] struct.

## Namespace/attribute resolution stage

This stage resolves namespace declarations found in a stream of [`RawEvent`]
structs and emits [`Event`] structs.

Together with the validation of the logical stage, this provides full
conformity checks according to XML 1.0 and Namespaces for XML 1.0. Both stages
are combined in the [`Parser`] struct.

The downside of using this stage is added processing cost, because
considerable dynamic allocations need to be performed per-element (for
attribute hash maps). In addition, information about the prefixes used to
declare namespaces is lost (but nothing should rely on those anyway).
*/

mod common;
mod namespaces;
mod raw;

#[doc(inline)]
pub use self::common::*;
#[doc(inline)]
pub use self::namespaces::{Event, QName};
#[doc(inline)]
pub use self::raw::{RawEvent, RawParser, RawQName};

use self::namespaces::NamespaceResolver;

/**
# Non-blocking restricted XML 1.0 parser

The [`Parser`] allows parsing XML documents as they arrive in the application,
giving back control to the caller immediately when not enough data is available
for processing. This is especially useful when streaming data from sockets.

To read events from the `Parser` after feeding data, use its [`Parse`] trait.

## Example

```
use rxml::{Parser, Parse, Error, Event, XmlVersion, error::EndOrError};
let doc = b"<?xml version='1.0'?><hello>World!</hello>";
let mut fp = Parser::new();
// We expect a NeedMoreData, because the XML declaration is not complete yet
assert!(matches!(
	fp.parse(&mut &doc[..10], false).err().unwrap(),
	EndOrError::NeedMoreData,
));

// Now we pass the XML declaration (and some), so we expect a corresponding
// event
let ev = fp.parse(&mut &doc[10..25], false);
assert!(matches!(ev.unwrap().unwrap(), Event::XmlDeclaration(_, XmlVersion::V1_0)));
```

In contrast to a [`RawParser`], the [`Parser`] enforces well-formedness and
namespace-well-formedness.

   [`rxml`]: crate
*/
#[derive(Debug)]
pub struct Parser {
	inner: RawParser,
	resolver: NamespaceResolver,
}

impl Parser {
	/// Create a new parser with default settings.
	pub fn new() -> Self {
		Self::default()
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
		self.inner.set_text_buffering(enabled)
	}

	/// Return whether text buffering is enabled.
	///
	/// See [`set_text_buffering`][`Self::set_text_buffering`].
	#[inline(always)]
	pub fn text_buffering(&self) -> bool {
		self.inner.text_buffering()
	}
}

impl Default for Parser {
	fn default() -> Self {
		Self::with_options(Options::default())
	}
}

impl WithOptions for Parser {
	fn with_options(options: Options) -> Self {
		Self {
			inner: RawParser::with_options((&options).into()),
			resolver: if let Some(ctx) = options.context {
				NamespaceResolver::with_context(ctx)
			} else {
				NamespaceResolver::new()
			},
		}
	}
}

impl Parse for Parser {
	type Output = Event;

	fn parse(&mut self, r: &mut &[u8], at_eof: bool) -> Result<Option<Self::Output>> {
		let inner = &mut self.inner;
		self.resolver.next(|| inner.parse(r, at_eof))
	}

	fn release_temporaries(&mut self) {
		self.inner.release_temporaries();
		self.resolver.context().release_temporaries();
	}
}
