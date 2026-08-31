#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(clippy::manual_range_contains, clippy::tabs_in_doc_comments)]
#![no_std]
/*!
# Restricted XML parsing and encoding

This crate provides "restricted" parsing and encoding of XML 1.0 documents
with namespacing.

## Features (some call them restrictions)

* No external resources
* No custom entities
* No DTD whatsoever
* No processing instructions
* Optional (disabled-by-default) support for comments
* UTF-8 only
* Namespacing-well-formedness enforced
* XML 1.0 only
* Streamed parsing (parser emits a subset of SAX events)
* Streamed encoding
* Parser can be driven push- and pull-based
* Tokio-based asynchronicity supported (see [`AsyncReader`]).

See [`crate::_spec`] for additional documentation on what exactly
"restricted XML" means.

## Examples

### Parse data from byte slices

To parse a XML document from a byte slice (or a series of byte slices), you
can use the [`Parser`] with the [`Parse`] trait directly:

```
use rxml::{Parser, Parse, Error, Event, XmlVersion};
use std::io;
let mut doc = &b"<?xml version='1.0'?><hello>World!</hello>"[..];
let mut fp = Parser::new();
while doc.len() > 0 {
	let ev = fp.parse(&mut doc, true);  // true = doc contains the entire document
	println!("got event: {:?}", ev);
}
```

### Parse data from a standard library reader

To parse a XML document from a [`std::io::BufRead`] struct, you can use the
[`Reader`].

*/
#![cfg_attr(
	not(feature = "std"),
	doc = "Because that feature was not enabled at doc build time, the example cannot be tested.\n\n```ignore\n"
)]
#![cfg_attr(feature = "std", doc = "\n```\n")]
/*!
# use std::io::BufReader;
# let file = &mut &b"<?xml version='1.0'?><hello>World!</hello>"[..];
// let file = std::fs::File::open(..).unwrap();
let reader = BufReader::new(file);
let mut reader = rxml::Reader::new(reader);
let result = rxml::as_eof_flag(reader.read_all(|ev| {
	println!("got event: {:?}", ev);
}));
assert_eq!(result.unwrap(), true);  // true indicates eof
```

### Parse data using `tokio`

To parse a XML document from a [`tokio::io::AsyncBufRead`] struct, you can use
the [`AsyncReader`].

This requires the `tokio` feature.
*/
#![cfg_attr(
	not(feature = "tokio"),
	doc = "Because that feature was not enabled at doc build time, the example cannot be tested.\n\n```ignore\n"
)]
#![cfg_attr(feature = "tokio", doc = "\n```\n")]
/*!
# use tokio::io::AsyncRead;
use rxml::{AsyncReader, Error, Event, XmlVersion};
# tokio_test::block_on(async {
# let sock = &mut &b"<?xml version='1.0'?><hello>World!</hello>"[..];
// let sock = ..;
let reader = tokio::io::BufReader::new(sock);
let mut reader = AsyncReader::new(reader);
// we expect the first event to be the XML declaration
let ev = reader.read().await;
assert!(matches!(ev, Ok(Some(Event::XmlDeclaration(_, XmlVersion::V1_0)))));
# })
```

## Feature flags

- `macros`: Enable macros to convert `&str` to `&NameStr` and `&NcNameStr`,
  respectively.
- `compact_str` (default): Enable the use of
  [`compact_str`](https://crates.io/crates/compact_str) for some string types
  to avoid allocations and conserve heap memory.
- `tokio` (default): Enable `AsyncReader` and related types.
- `stream`: Add a `futures::Stream` implementation to `AsyncReader`. Implies
  `tokio`.
- `shared_ns`: Allow deduplication of namespace URIs within and across
  parsers.
- `std` (default): Enable `Reader` and related types which depend on
  `std::io`.
- `extra-platforms`: Use the Arc implementation from
  [`portable-atomic-util`](https://crates.io/crates/portable-atomic-util).
  Note that `extra-platforms` and `shared_ns` cannot be enabled at the same
  time currently. Enabling both will cause the `shared_ns` feature to be
  implicitly and silently disabled.
*/
extern crate alloc;
#[cfg(any(test, feature = "std"))]
extern crate std;
mod context;
pub mod error;
mod lexer;
pub mod parser;
#[cfg(any(test, feature = "std"))]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
mod reader;
pub mod strings;
pub mod writer;
pub mod xml_lang;
pub mod xml_map;

#[cfg(test)]
mod tests;

#[cfg(doc)]
pub mod _spec;

pub use context::Context;
#[doc(inline)]
pub use error::{Error, Result};
#[doc(inline)]
pub use parser::{
	Event, Options, Parse, Parser, QName, RawEvent, RawParser, RawQName, WithOptions, XmlVersion,
	XMLNS_XML, XMLNS_XMLNS,
};
#[cfg(any(test, feature = "std"))]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
#[doc(inline)]
pub use reader::{as_eof_flag, GenericReader, RawReader, Reader};
pub use strings::{Name, NameStr, Namespace, NcName, NcNameStr};
#[doc(inline)]
pub use writer::{Encoder, Item, PREFIX_XML, PREFIX_XMLNS};
#[doc(inline)]
pub use xml_lang::XmlLangTracker;
#[doc(inline)]
pub use xml_map::{AttrMap, XmlMap};

#[cfg(feature = "macros")]
#[cfg_attr(docsrs, doc(cfg(feature = "macros")))]
#[deprecated(since = "0.11.0", note = "use the re-exported macros directly.")]
#[doc(hidden)]
pub use rxml_proc;

#[cfg(feature = "macros")]
#[doc(hidden)]
pub mod exports {
	pub use rxml_proc;
}

pub use bytes;

/**
Compile-time conversion of a string literal to [`NameStr`]

Convert a string literal into a `NameStr`, while asserting its compliance
at compile time.

# Example

```rust
use rxml::{NameStr, xml_name};

const FORBIDDEN: &'static NameStr = xml_name!("xmlns:xml");
```

Invalid values are rejected at compile-time:

```rust,compile_fail
# use rxml::{NameStr, xml_name};
const INVALID: &'static NameStr = xml_name!("foo bar");
```
*/
#[cfg(feature = "macros")]
#[cfg_attr(docsrs, doc(cfg(feature = "macros")))]
#[macro_export]
macro_rules! xml_name {
	($s:literal) => {
		// SAFETY: we pass the rxml crate as second argument, so the types
		// referenced by the macro are the correct ones.
		unsafe { $crate::exports::rxml_proc::xml_name!($s, $crate) }
	};
}

/**
Compile-time conversion of a string literal to [`NcNameStr`]

Convert a string literal into a `NcNameStr`, while asserting its compliance
at compile time.

# Example

```rust
use rxml::{NcNameStr, xml_ncname};

const XML_PREFIX: &'static NcNameStr = xml_ncname!("xml");
```

Invalid values are rejected at compile-time:

```rust,compile_fail
# use rxml::{NcNameStr, xml_ncname};
const INVALID: &'static NcNameStr = xml_ncname!("xmlns:xml");
```
*/
#[cfg(feature = "macros")]
#[cfg_attr(docsrs, doc(cfg(feature = "macros")))]
#[macro_export]
macro_rules! xml_ncname {
	($s:literal) => {
		// SAFETY: we pass the rxml crate as second argument, so the types
		// referenced by the macro are the correct ones.
		unsafe { $crate::exports::rxml_proc::xml_ncname!($s, $crate) }
	};
}

#[cfg(feature = "tokio")]
mod future;

#[cfg(feature = "tokio")]
#[doc(inline)]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub use future::{AsyncRawReader, AsyncReader, GenericAsyncReader};

/// Package version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
