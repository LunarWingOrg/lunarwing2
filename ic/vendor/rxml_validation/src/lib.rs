#![no_std]
#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(clippy::manual_range_contains)]
/*!
# Strongly-typed strings for use with XML 1.0 documents

This crate defines various string- and str-like types which represent pieces
of text as they may occur in XML documents. These types are checked to contain
only text which conforms to the respective grammar in the XML specifications.

This allows to carry information about the checking which already took place
in the parser to the application, avoiding the need to execute checks multiple
times.

This is a supplementary crate for [`rxml`](https://docs.rs/rxml). It is
factored out of the main crate to support
[`rxml_proc`](https://docs.rs/rxml_proc), a crate of macros which allow
compile-time validation and typing of XML strings. All types defined in this
crate are re-exported in `rxml`; if you depend on `rxml`, you can use the
types from there directly.

If the `std` feature is *not* enabled (it is enabled by default), this crate
can be used in `no_std` environments.

## Type Overview

- [`Name`] and [`NameStr`] represent the `Name` production and can be used
  for element and attribute names before namespace prefix expansion.
- [`NcName`] and [`NcNameStr`] represent the `Name` production but without a
  colon inside; they are used for localnames after prefix expansion and to
  carry the prefixes themselves.

## Construction

In general, values are constructed using the [`core::convert::TryInto`]
trait, from other string types or `str`. Supported source types are:

* [`String`] (copies)
* [`compact_str::CompactString`] (moves)
* [`str`] (copies for all types except the slice types)

**Note:** If the `compact_str` feature is *not* enabled, all string types use
the normal [`std::string::String`] type instead.

In addition, converting from [`NcName`] to [`Name`] is possible without extra
checking and is thus possible through `.into()` (and likewise for the
corresponding str types).

The inverse directions are only available through `try_into`.

## When to use rxml_validation vs. rxml?

You should use this crate (`rxml_validation`) whenever you only need to
validate strings against rules present in XML, without actually parsing or
serialising XML data. In that case, this crate is a much lighter choice and
it can be used in `no_std` environments.
*/
extern crate alloc;

use core::fmt;

mod strings;

pub mod selectors;

use selectors::CharSelector;

#[doc(inline)]
pub use strings::{CompactString, Name, NameStr, NcName, NcNameStr};

/**
Error condition from validating an XML string.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
	/// A Name or NCName was empty.
	EmptyName,

	/// An invalid character was encountered.
	///
	/// This variant contains the character as data.
	InvalidChar(char),

	/// One side of the colon in a name was empty.
	///
	/// The contents are implementation details.
	EmptyNamePart,

	/// More than one colon encountered in a name.
	///
	/// The contents are implementation details.
	MultiColonName,

	/// Local name does not conform to Name production (invalid start char)
	InvalidLocalName,
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			Self::EmptyName => f.write_str("Name and NCName must not be empty"),
			Self::InvalidChar(c) => write!(f, "character U+{:04x} is not allowed", *c as u32),
			Self::EmptyNamePart => f.write_str("empty string on one side of the colon"),
			Self::MultiColonName => f.write_str("more than one colon"),
			Self::InvalidLocalName => f.write_str("local name is invalid"),
		}
	}
}

impl core::error::Error for Error {}

/**
Check whether a str is a valid XML 1.0 Name

# Example

```rust
use rxml_validation::{validate_name, Error};

assert!(validate_name("foobar").is_ok());
assert!(validate_name("foo:bar").is_ok());
assert!(matches!(validate_name("foo bar"), Err(Error::InvalidChar(' '))));
assert!(matches!(validate_name(""), Err(Error::EmptyName)));
*/
pub fn validate_name(s: &str) -> Result<(), Error> {
	let mut chars = s.chars();
	match chars.next() {
		// must have at least one char
		None => return Err(Error::EmptyName),
		Some(c) => {
			if !selectors::CLASS_XML_NAMESTART.select(c) {
				return Err(Error::InvalidChar(c));
			}
		}
	}
	for ch in chars {
		if !selectors::CLASS_XML_NAME.select(ch) {
			return Err(Error::InvalidChar(ch));
		}
	}
	Ok(())
}

/**
Check whether a str is a valid XML 1.0 Name, without colons.

# Example

```rust
use rxml_validation::{validate_ncname, Error};

assert!(validate_ncname("foobar").is_ok());
assert!(matches!(validate_ncname("foo:bar"), Err(Error::InvalidChar(':'))));
assert!(matches!(validate_ncname(""), Err(Error::EmptyName)));
*/
pub fn validate_ncname(s: &str) -> Result<(), Error> {
	let mut chars = s.chars();
	match chars.next() {
		// must have at least one char
		None => return Err(Error::EmptyName),
		Some(c) => {
			if !selectors::CLASS_XML_NAMESTART.select(c) || c == ':' {
				return Err(Error::InvalidChar(c));
			}
		}
	}
	for ch in chars {
		if !selectors::CLASS_XML_NAME.select(ch) || ch == ':' {
			return Err(Error::InvalidChar(ch));
		}
	}
	Ok(())
}

/**
Check whether a str is valid XML 1.0 CData.

There exists no specific string type for CData, because it is almost identical
to Rust strings and encoding even a validated CData string into XML requires
extra steps because it may contain characters which need escaping when written
into an XML document.

# Example

```rust
use rxml_validation::{validate_cdata, Error};

assert!(validate_cdata("foo bar baz <fnord!>").is_ok());
assert!(matches!(validate_cdata("\x01"), Err(Error::InvalidChar('\x01'))));
*/
pub fn validate_cdata(s: &str) -> Result<(), Error> {
	let s = s.as_bytes();
	for i in 0..s.len() {
		let b = s[i];
		if b < 0x09 || b == 0x0b || b == 0x0c || (b >= 0x0e && b <= 0x1f) {
			return Err(Error::InvalidChar(b.into()));
		}
		if b == 0xbe || b == 0xbf {
			if i >= 2 && s[i - 2] == 0xef && s[i - 1] == 0xbf {
				// U+FFFE or U+FFFF
				let bit = (b & 0x01) as u32;
				// SAFETY: we are passing only 16 bits and the upper
				// nibble is set to all ones, so this is within the bounds
				// of a unicode code point and not a surrogate.
				let ch = unsafe { char::from_u32_unchecked(0xfffe | bit) };
				return Err(Error::InvalidChar(ch));
			}
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::string::String;

	#[test]
	fn test_cdata_smoketest() {
		assert!(validate_cdata("foo bar baz http://<xyz>").is_ok());
		assert!(validate_cdata("\u{ffff}").is_err());
	}

	#[test]
	fn test_name_smoketest() {
		assert!(validate_name("foobar").is_ok());
		assert!(validate_name("foo:bar").is_ok());
		assert!(validate_name("").is_err());
		assert!(validate_name("foo bar baz http://<xyz>").is_err());
		assert!(validate_name("\u{ffff}").is_err());
	}

	#[test]
	fn test_ncname_smoketest() {
		assert!(validate_ncname("foobar").is_ok());
		assert!(validate_ncname("foo:bar").is_err());
		assert!(validate_ncname("").is_err());
		assert!(validate_ncname("foo bar baz http://<xyz>").is_err());
		assert!(validate_ncname("\u{ffff}").is_err());
	}

	#[test]
	fn test_validate_cdata_is_equivalent_to_nonchar_class() {
		let mut buf = String::with_capacity(4);
		for cp in 0x0..=0x10ffffu32 {
			if let Some(ch) = char::from_u32(cp) {
				buf.clear();
				buf.push(ch);
				if selectors::CLASS_XML_NONCHAR.select(ch) {
					match validate_cdata(&buf) {
						Err(Error::InvalidChar(v)) => {
							assert_eq!(v, ch);
						}
						other => panic!(
							"validate_cdata accepts {:?} (ch={:?}) which is rejected by CLASS_XML_NONCHAR: {:?}",
							buf, ch, other
						),
					}
				} else {
					match validate_cdata(&buf) {
						Ok(()) => (),
						other => panic!(
							"validate_cdata rejects {:?} (ch={:?}) which is accepted by CLASS_XML_NONCHAR: {:?}",
							buf, ch, other
						),
					}
				}
			}
		}
	}
}
