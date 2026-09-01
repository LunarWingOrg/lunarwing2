/*!
# Strongly-typed strings for use with XML 1.0 documents

These are mostly re-exported from [`rxml_validation`]. See that crate's
documentation for an overview over the different types and their use cases.

## Construction

In addition to the construction methods described in [`rxml_validation`],
[`str`]-like references ([`&NameStr`][`NameStr`], [`&NcNameStr`][`NcNameStr`])
can be created from a string literal, using the macros offered when this crate
is built with the `macros` feature:
[`xml_name!`][`crate::xml_name`], [`xml_ncname!`][`crate::xml_ncname`].
*/
use alloc::borrow::ToOwned;
use alloc::string::String;
#[cfg(not(feature = "extra-platforms"))]
use alloc::sync::Arc;
use core::borrow::Borrow;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::ops::Deref;
#[cfg(feature = "extra-platforms")]
use portable_atomic_util::Arc;

pub use rxml_validation::{CompactString, Error, Name, NameStr, NcName, NcNameStr};

use crate::error::{Error as XmlError, ErrorContext};

enum Inner<'x> {
	Owned(Arc<String>),
	Shared(&'x str),
}

/// An XML namespace name.
///
/// This struct stores an XML namespace name as string. This storage is always
/// copy-on-write, but may be *borrowed* or *shared*. In the *borrowed* case,
/// the URI is a `&str` the lifetime of the `Namespace` is the lifetime of the
/// borrow. In the *shared* case, the namespace URI is stored in an
/// `Arc<String>` and the lifetime of `Namespace` may be any lifetime,
/// including `'static`.
///
/// [`make_mut()`][`Self::make_mut`] can be used to force conversion to
/// uniquely-owned *shared* storage (i.e. the string is copied, if necessary,
/// into a fresh `Arc` in order to allow mutation), implementing copy-on-write
/// semantics.
///
/// Almost all URIs are also valid namespace names, with the prominent
/// exception of the empty URI: namespace names in XML must not be empty. The
/// empty namespace name signals that an element is not namespaced. For
/// purposes of comparison and similar operations, we can treat the empty
/// namespace name as just another namespace, which is why it is an allowed
/// value for this struct[^why-allowed].
///
/// Cloning a `Namespace` is always cheap.
///
///    [^why-allowed]: Purists may argue that allowing the empty string as a
///     value for `Namespace`, even though the empty string not actually being
///     a valid namespace name, is a bad thing.
///
///     An initial draft of the `Namespace` struct indeed rejected the empty
///     string. This came with likely runtime and mental overhead, because a
///     lot of casts would now be fallible, having to return
///     `Option<Namespace>` instead. In experimentation, it then turned out
///     that almost everywhere where `Namespace` would now be used, one would
///     have to in fact use `Option<Namespace>`. Even in strongly-namespaced
///     downstream crates like `xso`, there was no place where one would have
///     to have assert that a `Namespace` does not refer to unnamespaced
///     elements; for all practical purposes, unnamespaced elements are just
///     in a special namespace which compares differently to all other
///     namespaces.
///
///     The empty string does just that. We thus allow the empty string as
///     value and provide helpers to check for empty/non-empty as well as
///     constants to quickly create a `Namespace` refering to unnamespaced
///     elements.
///
///     In addition, excluding the empty string as valid value also has no
///     benefits from a niche perspective (unlike for example when one can use
///     `NonZeroU8`, which brings a niche for `Option<_>`).
pub struct Namespace<'x>(Inner<'x>);

const fn xml() -> Namespace<'static> {
	Namespace(Inner::Shared(crate::XMLNS_XML))
}

const fn xmlns() -> Namespace<'static> {
	Namespace(Inner::Shared(crate::XMLNS_XMLNS))
}

const fn none() -> Namespace<'static> {
	Namespace(Inner::Shared(crate::parser::XMLNS_UNNAMESPACED))
}

impl Namespace<'static> {
	/// `Namespace` representing the built-in XML namespace.
	///
	/// See also [`crate::XMLNS_XML`].
	pub const XML: Namespace<'static> = xml();

	/// `Namespace` representing the built-in XML namespacing namespace.
	///
	/// See also [`crate::XMLNS_XMLNS`].
	pub const XMLNS: Namespace<'static> = xmlns();

	/// `Namespace` representing the empty namespace name.
	pub const NONE: Namespace<'static> = none();

	/// Construct a namespace name for the built-in XML namespace.
	///
	/// This function does not allocate.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::strings::Namespace;
	/// let ns = Namespace::xml();
	/// assert_eq!(ns, rxml::XMLNS_XML);
	/// ```
	#[inline(always)]
	pub fn xml() -> &'static Self {
		static RESULT: Namespace = Namespace::XML;
		&RESULT
	}

	/// Construct a namespace name for the built-in XML namespacing namespace.
	///
	/// This function does not allocate.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::strings::Namespace;
	/// let ns = Namespace::xmlns();
	/// assert_eq!(ns, rxml::XMLNS_XMLNS);
	/// ```
	#[inline(always)]
	pub fn xmlns() -> &'static Self {
		static RESULT: Namespace = Namespace::XMLNS;
		&RESULT
	}

	/// Construct an empty namespace name, representing unnamespaced elements.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::strings::Namespace;
	/// let ns = Namespace::none();
	/// assert_eq!(ns, "");
	/// ```
	#[inline(always)]
	pub fn none() -> &'static Self {
		static RESULT: Namespace = Namespace::NONE;
		&RESULT
	}

	/// Attempts to deduplicate the given namespace name with a set of
	/// statically compiled well-known namespaces.
	///
	/// If no matching statically compiled namespace is found, `None` is
	/// returned.
	///
	/// Otherwise, a `Namespace` is created without allocating and without
	/// sharing data with `s`.
	pub fn try_share_static(s: &str) -> Option<Self> {
		if s.is_empty() {
			return Some(Self::NONE);
		}
		if s == crate::XMLNS_XML {
			return Some(Self::XML);
		}
		if s == crate::XMLNS_XMLNS {
			return Some(Self::XMLNS);
		}
		None
	}
}

impl<'x> Namespace<'x> {
	/// Construct a namespace name for a custom namespace from a string
	/// reference.
	///
	/// This function is provided for use in `const` contexts, while normally
	/// you would use the `From<&str>` implementation.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::strings::Namespace;
	/// static NS: Namespace = Namespace::from_str("jabber:client");
	/// assert_eq!(NS, "jabber:client");
	/// ```
	pub const fn from_str(s: &'x str) -> Self {
		Self(Inner::Shared(s))
	}

	/// Reborrow this namespace to return a `Namespace` bound to its lifetime.
	///
	/// If this namespace is owned, the contained `Arc` is cloned and the clone
	/// is stored in the result. That way, the actual namespace URI is not
	/// cloned even if the returned namespace is cloned or turned into a
	/// static namespace via [`into_static()`][`Self::into_static`].
	pub fn borrow(&self) -> Namespace<'_> {
		match self.0 {
			Inner::Shared(v) => Namespace(Inner::Shared(v)),
			Inner::Owned(ref v) => Namespace(Inner::Owned(Arc::clone(v))),
		}
	}

	/// Convert to uniquely-owned shared storage.
	///
	/// If `self` is currently borrowed, it is converted to shared storage by
	/// copying the borrowed data into an `Arc<String>`. If `self` is already
	/// shared storage and it holds the only Arc reference, this is a no-op.
	/// Otherwise, the string is cloned to create a fresh Arc which is then
	/// mutably accessible.
	///
	/// Subsequent calls to `make_mut` are O(1), unless `clone()` has been
	/// called inbetween and the clones still exist.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::strings::Namespace;
	/// let mut ns1 = Namespace::XMLNS;
	/// assert_eq!(ns1, Namespace::XMLNS);
	/// ns1.make_mut().push_str("#hello");
	/// assert_eq!(ns1, "http://www.w3.org/2000/xmlns/#hello");
	/// assert_ne!(ns1, Namespace::XMLNS);
	/// ```
	pub fn make_mut(&mut self) -> &mut String {
		match self.0 {
			Inner::Shared(v) => {
				let mut tmp = Inner::Owned(Arc::new(v.to_owned()));
				core::mem::swap(&mut self.0, &mut tmp);
				let Inner::Owned(ref mut v) = self.0 else {
					unreachable!()
				};
				Arc::make_mut(v)
			}
			Inner::Owned(ref mut v) => Arc::make_mut(v),
		}
	}

	/// Convert the value into a static `Namespace`, copying borrowed data if
	/// necessary.
	///
	/// This is a no-op if the namespace URI is in *shared* storage already.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::strings::Namespace;
	/// # use std::sync::Mutex;
	/// static SLOT: Mutex<Namespace<'static>> = Mutex::new(Namespace::NONE);
	/// let mut s = String::new();
	/// s.push_str("hello");
	/// let ns = Namespace::from(s.as_str());
	/// // *SLOT.lock().unwrap() = ns;
	/// // ↑ does not work because lifetimes.
	/// *SLOT.lock().unwrap() = ns.into_static();
	/// // ↑ copies the borrowed data.
	/// // Contrived example. It would be more efficient to do
	/// // Namespace::from(s) in this case, which provides a
	/// // Namespace<'static> straight away.
	/// ```
	pub fn into_static(self) -> Namespace<'static> {
		Namespace(match self.0 {
			Inner::Shared(v) => match Namespace::try_share_static(v) {
				Some(v) => return v,
				None => Inner::Owned(Arc::new(v.to_owned())),
			},
			Inner::Owned(v) => Inner::Owned(v),
		})
	}

	/// Return a reference to the inner namespace name if it is not empty or
	/// `None` otherwise.
	///
	/// XML namespace names must not be empty. The empty namespace name
	/// denotes unnamespaced elements.
	///
	/// It should be rarely necessary to use this function.
	pub fn as_namespace_name(&self) -> Option<&str> {
		let s = self.deref();
		if s.is_empty() {
			None
		} else {
			Some(s)
		}
	}

	/// Return true if this `Namespace` is the empty namespace name.
	///
	/// The empty namespace name is not a valid namespace and is thus used to
	/// identify unnamespaced elements.
	pub fn is_none(&self) -> bool {
		self.deref().is_empty()
	}

	/// Return true if this `Namespace` is a non-empty namespace name.
	pub fn is_some(&self) -> bool {
		!self.deref().is_empty()
	}

	/// Return a reference to the string slice inside this [`Namespace`].
	pub fn as_str(&self) -> &str {
		self.deref()
	}
}

impl Deref for Namespace<'_> {
	type Target = str;

	fn deref(&self) -> &Self::Target {
		match self.0 {
			Inner::Shared(v) => v,
			Inner::Owned(ref v) => v.deref(),
		}
	}
}

impl AsRef<str> for Namespace<'_> {
	fn as_ref(&self) -> &str {
		self.deref()
	}
}

impl Borrow<str> for Namespace<'_> {
	fn borrow(&self) -> &str {
		self.deref()
	}
}

impl Clone for Namespace<'_> {
	fn clone(&self) -> Self {
		Self(match self.0 {
			Inner::Shared(v) => Inner::Shared(v),
			Inner::Owned(ref v) => Inner::Owned(v.clone()),
		})
	}
}

impl fmt::Debug for Namespace<'_> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let mut wrapper = match self.0 {
			Inner::Shared(_) => f.debug_tuple("Namespace<'x>"),
			Inner::Owned(_) => f.debug_tuple("Namespace<'static>"),
		};
		wrapper.field(&self.deref()).finish()
	}
}

impl fmt::Display for Namespace<'_> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		<str as fmt::Display>::fmt(self.deref(), f)
	}
}

impl Eq for Namespace<'_> {}

impl From<Namespace<'_>> for String {
	fn from(other: Namespace) -> Self {
		match other.0 {
			Inner::Owned(v) => Arc::unwrap_or_clone(v),
			Inner::Shared(v) => v.to_owned(),
		}
	}
}

impl From<Namespace<'_>> for Arc<String> {
	fn from(other: Namespace) -> Self {
		match other.0 {
			Inner::Owned(v) => v,
			Inner::Shared(v) => Arc::new(v.to_owned()),
		}
	}
}

impl From<Name> for Namespace<'static> {
	fn from(other: Name) -> Self {
		Self(Inner::Owned(Arc::new(other.into())))
	}
}

impl From<NcName> for Namespace<'static> {
	fn from(other: NcName) -> Self {
		Self(Inner::Owned(Arc::new(other.into())))
	}
}

impl<'x> From<&'x Name> for Namespace<'x> {
	fn from(other: &'x Name) -> Self {
		Self(Inner::Shared(other.as_ref()))
	}
}

impl<'x> From<&'x NcName> for Namespace<'x> {
	fn from(other: &'x NcName) -> Self {
		Self(Inner::Shared(other.as_ref()))
	}
}

impl<'x> From<&'x str> for Namespace<'x> {
	fn from(other: &'x str) -> Self {
		Self(Inner::Shared(other))
	}
}

impl From<String> for Namespace<'static> {
	fn from(other: String) -> Self {
		Self(Inner::Owned(Arc::new(other)))
	}
}

impl From<Arc<String>> for Namespace<'static> {
	fn from(other: Arc<String>) -> Self {
		Self(Inner::Owned(other))
	}
}

impl Hash for Namespace<'_> {
	fn hash<H: Hasher>(&self, h: &mut H) {
		self.deref().hash(h)
	}
}

impl Ord for Namespace<'_> {
	fn cmp(&self, other: &Namespace<'_>) -> Ordering {
		self.deref().cmp(other.deref())
	}
}

impl PartialEq for Namespace<'_> {
	fn eq(&self, other: &Namespace<'_>) -> bool {
		self.deref() == other.deref()
	}
}

impl PartialEq<&str> for Namespace<'_> {
	fn eq(&self, other: &&str) -> bool {
		self.deref() == *other
	}
}

impl PartialEq<Namespace<'_>> for &str {
	fn eq(&self, other: &Namespace) -> bool {
		*self == other.deref()
	}
}

impl PartialEq<str> for Namespace<'_> {
	fn eq(&self, other: &str) -> bool {
		self.deref() == other
	}
}

impl PartialEq<Namespace<'_>> for str {
	fn eq(&self, other: &Namespace) -> bool {
		self == other.deref()
	}
}

impl PartialOrd for Namespace<'_> {
	fn partial_cmp(&self, other: &Namespace) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

/**
Check whether a str is valid XML 1.0 CData

# Example

```rust
use rxml::Error;
use rxml::strings::validate_cdata;

assert!(validate_cdata("foo bar baz <fnord!>").is_ok());
assert!(matches!(validate_cdata("\x01"), Err(Error::UnexpectedChar(_, '\x01', _))));
*/
pub fn validate_cdata(s: &str) -> Result<(), XmlError> {
	rxml_validation::validate_cdata(s).map_err(|e| XmlError::from_validation(e, None))
}

/**
Check whether a str is a valid XML 1.0 Name

**Note:** This does *not* enforce that the name contains only a single colon.

# Example

```rust
use rxml::Error;
use rxml::strings::validate_name;

assert!(validate_name("foobar").is_ok());
assert!(validate_name("foo:bar").is_ok());
assert!(matches!(validate_name("foo bar"), Err(Error::UnexpectedChar(_, ' ', _))));
assert!(matches!(validate_name(""), Err(Error::InvalidSyntax(_))));
*/
pub fn validate_name(s: &str) -> Result<(), XmlError> {
	rxml_validation::validate_name(s)
		.map_err(|e| XmlError::from_validation(e, Some(ErrorContext::Name)))
}

/**
Check whether a str is a valid XML 1.0 Name, without colons.

# Example

```rust
use rxml::Error;
use rxml::strings::validate_ncname;

assert!(validate_ncname("foobar").is_ok());
assert!(matches!(validate_ncname("foo:bar"), Err(Error::MultiColonName(_))));
assert!(matches!(validate_ncname(""), Err(Error::EmptyNamePart(_))));
*/
pub fn validate_ncname(s: &str) -> Result<(), XmlError> {
	match rxml_validation::validate_ncname(s)
		.map_err(|e| XmlError::from_validation(e, Some(ErrorContext::Name)))
	{
		Err(XmlError::UnexpectedChar(ctx, ':', _)) => Err(XmlError::MultiColonName(ctx)),
		Err(XmlError::InvalidSyntax(_)) => Err(XmlError::EmptyNamePart(None)),
		other => other,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn can_slice_namespace_name() {
		let nsn = Namespace::xml();
		assert_eq!(&nsn[..4], "http");
	}

	#[test]
	fn namespace_size_leq_string_size() {
		let ns_sz = core::mem::size_of::<Namespace>();
		let string_sz = core::mem::size_of::<String>();
		assert!(ns_sz <= string_sz, "{ns_sz} <= {string_sz}");
	}

	#[test]
	fn option_namespace_size_leq_option_string_size() {
		let ns_sz = core::mem::size_of::<Option<Namespace>>();
		let string_sz = core::mem::size_of::<Option<String>>();
		assert!(ns_sz <= string_sz, "{ns_sz} <= {string_sz}");
	}
}
