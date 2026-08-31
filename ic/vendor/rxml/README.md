# `rxml` — Restricted, minimalistic XML 1.0 parser

This crate provides "restricted" parsing of XML 1.0 documents with
namespacing.

[![crate badge](https://img.shields.io/crates/v/rxml.svg)](https://crates.io/crates/rxml) [![docs badge](https://docs.rs/rxml/badge.svg)](https://docs.rs/rxml/)

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
* Tokio-based asynchronicity supported via the `async` feature and [`AsyncReader`].

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

```
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

```
# use tokio::io::AsyncRead;
use rxml::{AsyncReader, Error, Event, XmlVersion};
# tokio_test::block_on(async {
# let sock = &mut &b"<?xml version='1.0'?><hello>World!</hello>"[..];
// let sock = ..;
let reader = tokio::io::BufReader::new(sock);
// this converts the doc into an tokio::io::AsyncRead
let mut reader = AsyncReader::new(reader);
// we expect the first event to be the XML declaration
let ev = reader.read().await;
assert!(matches!(ev.unwrap().unwrap(), Event::XmlDeclaration(_, XmlVersion::V1_0)));
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
