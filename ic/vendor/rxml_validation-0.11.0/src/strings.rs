use core::borrow::Borrow;
use core::cmp::{Ordering, PartialOrd};
use core::convert::{TryFrom, TryInto};
use core::fmt;
use core::ops::{
	Deref, Index, Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive,
};

#[cfg(feature = "std")]
use std::{
	borrow::{Cow, ToOwned},
	iter::FromIterator,
	ops::{Add, AddAssign, DerefMut},
};

/// Type alias to access the inner type used for [`Name`] and [`NcName`]
/// irrespective of feature flags.
///
/// This will be [`compact_str::CompactString`] if the `compact_str` feature
/// flag is enabled and [`String`] otherwise.
#[cfg(feature = "compact_str")]
pub type CompactString = compact_str::CompactString;

/// Type alias to access the inner type used for [`Name`] and [`NcName`]
/// irrespective of feature flags.
///
/// This will be `compact_str::CompactString` if the `compact_str` feature
/// flag is enabled and [`String`] otherwise.
#[cfg(all(not(feature = "compact_str"), feature = "std"))]
pub type CompactString = String;

use crate::selectors;
use crate::selectors::CharSelector;
use crate::Error;
use crate::{validate_name, validate_ncname};

macro_rules! rxml_unsafe_str_construct_doc {
	($name:ident, $other:ident) => {
		concat!(
			"Construct a `",
			stringify!($name),
			"` without enforcing anything\n",
			"\n",
			"# Safety\n",
			"\n",
			"The caller is responsible for ensuring that the passed [`",
			stringify!($other),
			"`] is in fact a valid `",
			stringify!($name),
			"`.\n",
		)
	};
}

macro_rules! rxml_safe_str_construct_doc {
	($name:ident, $other:ident, $more:expr) => {
		concat!(
			"Converts a [`",
			stringify!($other),
			"`] to a `",
			stringify!($name),
			"`.\n",
			"\n",
			"If the given `",
			stringify!($other),
			"` does not conform to the restrictions imposed by `",
			stringify!($name),
			"`, an error is returned.\n",
			$more
		)
	};
}

macro_rules! rxml_split_at_example {
	($borrowed:ty) => {
		concat!(
			"\n\n```\n",
			"# use std::convert::TryInto;\n",
			"# use rxml_validation::",
			stringify!($borrowed),
			";\n",
			"let value: &",
			stringify!($borrowed),
			" = \"foobar\".try_into().unwrap();\n",
			"let (lhs, rhs) = value.split_at(3);\n",
			"assert_eq!(lhs, \"foo\");\n",
			"assert_eq!(rhs, \"bar\");\n",
			"```\n",
		)
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_make_ascii_lowercase_example {
	($owned:ty, $borrowed:ty) => {
		concat!(
			"\n\n# Example\n\n```\n",
			"# use std::convert::TryInto;\n",
			"# use rxml_validation::{",
			stringify!($borrowed),
			", ",
			stringify!($owned),
			"};\n",
			"let mut owned: ",
			stringify!($owned),
			" = \"FÖöBar\".try_into().unwrap();\n",
			"let borrowed: &mut ",
			stringify!($borrowed),
			" = &mut owned;\n",
			"borrowed.make_ascii_lowercase();\n",
			"assert_eq!(borrowed, \"fÖöbar\");\n",
			"```\n",
		)
	};
}

#[cfg(not(feature = "std"))]
macro_rules! rxml_make_ascii_lowercase_example {
	($owned:ty, $borrowed:ty) => {
		""
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_make_ascii_uppercase_example {
	($owned:ty, $borrowed:ty) => {
		concat!(
			"\n\n# Example\n\n```\n",
			"# use std::convert::TryInto;\n",
			"# use rxml_validation::{",
			stringify!($borrowed),
			", ",
			stringify!($owned),
			"};\n",
			"let mut owned: ",
			stringify!($owned),
			" = \"FÖöBar\".try_into().unwrap();\n",
			"let borrowed: &mut ",
			stringify!($borrowed),
			" = &mut owned;\n",
			"borrowed.make_ascii_uppercase();\n",
			"assert_eq!(borrowed, \"FÖöBAR\");\n",
			"```\n",
		)
	};
}

#[cfg(not(feature = "std"))]
macro_rules! rxml_make_ascii_uppercase_example {
	($owned:ty, $borrowed:ty) => {
		""
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_split_off_panic_on_empty {
	() => {
		concat!(
			"\n",
			"# Panics\n",
			"\n",
			"If `idx` is 0 or equal to the length minus one, as the empty ",
			"string is not valid.\n",
		)
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_split_off_panics {
	(NcName) => {
		rxml_split_off_panic_on_empty!()
	};
	(Name) => {
		rxml_split_off_panic_on_empty!()
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_split_off_example {
	($ty:ident) => {
		concat!(
			"\n",
			"```\n",
			"# use std::convert::TryInto;\n",
			"# use rxml_validation::",
			stringify!($ty),
			";\n",
			"let mut value: ",
			stringify!($ty),
			" = \"foobar\".try_into().unwrap();\n",
			"let rhs: ",
			stringify!($ty),
			" = value.split_off(3);\n",
			"assert_eq!(value, \"foo\");\n",
			"assert_eq!(rhs, \"bar\");\n",
			"```\n",
		)
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_insert_str_example {
	($owned:ident, $borrowed:ident) => {
		concat!(
			"\n",
			"```\n",
			"# use std::convert::TryInto;\n",
			"# use rxml_validation::{",
			stringify!($owned),
			", ",
			stringify!($borrowed),
			"};\n",
			"let mut value: ",
			stringify!($owned),
			" = \"foobaz\".try_into().unwrap();\n",
			"let to_insert: &",
			stringify!($borrowed),
			" = \"bar\".try_into().unwrap();\n",
			"value.insert_str(3, to_insert);\n",
			"assert_eq!(value, \"foobarbaz\");\n",
			"```\n",
		)
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_push_str_example {
	($owned:ident, $borrowed:ident) => {
		concat!(
			"\n",
			"```\n",
			"# use std::convert::TryInto;\n",
			"# use rxml_validation::{",
			stringify!($owned),
			", ",
			stringify!($borrowed),
			"};\n",
			"let mut value: ",
			stringify!($owned),
			" = \"foobar\".try_into().unwrap();\n",
			"let to_append: &",
			stringify!($borrowed),
			" = \"baz\".try_into().unwrap();\n",
			"value.push_str(to_append);\n",
			"assert_eq!(value, \"foobarbaz\");\n",
			"```\n",
		)
	};
}

#[cfg(all(not(feature = "compact_str"), feature = "std"))]
macro_rules! rxml_non_compact_str_only_note {
	(CompactString) => {
		"\n# Note\nThis function is only available *without* the `compact_str` feature!\n"
	};
	($other:ident) => {
		""
	};
}

#[cfg(feature = "std")]
macro_rules! rxml_custom_string_type {
	(
		$(#[$outer:meta])*
		pub struct $name:ident($string:ident) use $check:ident => $borrowed:ident;
	) => {
		$(#[$outer])*
		#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord)]
		#[repr(transparent)]
		pub struct $name($string);

		impl $name {
			/// Extract the inner string and return it.
			pub fn into_inner(self) -> $string {
				self.0
			}

			/// Obtain a reference to the inner string slice.
			pub fn as_str(&self) -> &str {
				self.0.as_str()
			}

			/// Return the capacity, in bytes.
			pub fn capacity(&self) -> usize {
				self.0.capacity()
			}

			/// Inserts a slice into this typed string at a byte position.
			///
			/// This is an *O(n)* operation as it requires copying every
			/// element into the buffer.
			///
			/// # Panics
			///
			/// Panics if `idx` is larger than the string's length, or if it
			/// does not lie on a `char` boundary.
			///
			/// # Example
			///
			#[doc = rxml_insert_str_example!($name, $borrowed)]
			pub fn insert_str(&mut self, idx: usize, string: &$borrowed) {
				// CORRECTNESS: as $borrowed is a valid sub-slice of $name,
				// it is also valid at the start of $name -> we can insert it
				// at arbitrary positions.
				self.0.insert_str(idx, &string.0);
			}

			/// Return the length of this string in bytes.
			pub fn len(&self) -> usize {
				self.0.len()
			}

			/// Reserve capacity for at least `additional` bytes more than the
			/// current length.
			pub fn reserve(&mut self, additional: usize) {
				self.0.reserve(additional)
			}

			/// Reserve capacity for at least `additional` bytes more than the current
			/// length.
			///
			/// Unlike `reserve`, this will not over-allocate, ever.
			// ↓ CompactString does not have this.
			#[doc = rxml_non_compact_str_only_note!($string)]
			#[cfg(not(feature = "compact_str"))]
			pub fn reserve_exact(&mut self, additional: usize) {
				self.0.reserve_exact(additional)
			}

			/// Shrink the capacity of this string with a lower bound.
			pub fn shrink_to(&mut self, min_capacity: usize) {
				self.0.shrink_to(min_capacity)
			}

			/// Shrink the capacity of this string to match its length.
			pub fn shrink_to_fit(&mut self) {
				self.0.shrink_to_fit()
			}

			#[doc = rxml_unsafe_str_construct_doc!($name, str)]
			pub unsafe fn from_str_unchecked<T: AsRef<str>>(s: T) -> Self {
				Self(s.as_ref().into())
			}

			#[doc = rxml_unsafe_str_construct_doc!($name, String)]
			pub unsafe fn from_string_unchecked<T: Into<String>>(s: T) -> Self {
				Self(s.into().into())
			}

			#[cfg(feature = "compact_str")]
			#[allow(dead_code)]
			unsafe fn from_auto_unchecked(s: CompactString) -> Self {
				Self(s.into())
			}

			#[cfg(not(feature = "compact_str"))]
			#[allow(dead_code)]
			unsafe fn from_auto_unchecked(s: String) -> Self {
				Self(s.into())
			}

			#[doc = rxml_unsafe_str_construct_doc!($name, CompactString)]
			#[cfg(feature = "compact_str")]
			#[cfg_attr(docsrs, doc(cfg(feature = "compact_str")))]
			pub unsafe fn from_compact_str_unchecked<T: Into<CompactString>>(s: T) -> Self {
				Self(s.into().into())
			}

			unsafe fn from_native_unchecked(s: $string) -> Self {
				Self(s)
			}

			/// Appends a slice to this typed string.
			///
			/// This is an *O(n)* operation as it requires copying every
			/// element into the buffer.
			///
			/// # Example
			///
			#[doc = rxml_push_str_example!($name, $borrowed)]
			// CORRECTNESS:
			// For NcName and Name it holds that concatenations of these types
			// are correct.
			pub fn push_str(&mut self, v: &$borrowed) {
				self.0.push_str(&v.0)
			}
		}

		impl Deref for $name {
			type Target = $borrowed;

			fn deref(&self) -> &Self::Target {
				// SAFETY: $borrowed is assumed to use the same check; this is
				// enforced by using the pair macro.
				unsafe { $borrowed::from_str_unchecked(&self.0) }
			}
		}

		impl DerefMut for $name {
			fn deref_mut(&mut self) -> &mut Self::Target {
				// SAFETY: $borrowed is assumed to use the same check; this is
				// enforced by using the pair macro.
				unsafe { $borrowed::from_str_unchecked_mut(&mut self.0) }
			}
		}

		impl Borrow<$string> for $name {
			fn borrow(&self) -> &$string {
				&self.0
			}
		}

		impl Borrow<$borrowed> for $name {
			fn borrow(&self) -> &$borrowed {
				// SAFETY: $borrowed is assumed to use the same check; this is
				// enforced by using the pair macro.
				unsafe { $borrowed::from_str_unchecked(&self.0) }
			}
		}

		impl Borrow<str> for $name {
			fn borrow(&self) -> &str {
				&self.0
			}
		}

		impl AsRef<$string> for $name {
			fn as_ref(&self) -> &$string {
				&self.0
			}
		}

		impl AsRef<$borrowed> for $name {
			fn as_ref(&self) -> &$borrowed {
				// SAFETY: $borrowed is assumed to use the same check; this is
				// enforced by using the pair macro.
				unsafe { $borrowed::from_str_unchecked(&self.0) }
			}
		}

		impl AsRef<str> for $name {
			fn as_ref(&self) -> &str {
				&self.0
			}
		}

		impl PartialEq<str> for $name {
			fn eq(&self, other: &str) -> bool {
				self.0 == other
			}
		}

		// following the example of std::string::String, we define PartialEq
		// against the slice and the base type.
		impl PartialEq<$name> for str {
			fn eq(&self, other: &$name) -> bool {
				other.0 == self
			}
		}

		impl PartialEq<&str> for $name {
			fn eq(&self, other: &&str) -> bool {
				&self.0 == other
			}
		}

		impl PartialEq<$name> for &str {
			fn eq(&self, other: &$name) -> bool {
				other.0 == *self
			}
		}

		impl PartialEq<$borrowed> for $name {
			fn eq(&self, other: &$borrowed) -> bool {
				self.0 == &other.0
			}
		}

		impl PartialEq<$name> for $borrowed {
			fn eq(&self, other: &$name) -> bool {
				other.0 == &self.0
			}
		}

		impl PartialEq<&$borrowed> for $name {
			fn eq(&self, other: &&$borrowed) -> bool {
				self.0 == &other.0
			}
		}

		impl PartialEq<$name> for &$borrowed {
			fn eq(&self, other: &$name) -> bool {
				other.0 == &self.0
			}
		}

		impl PartialOrd<$name> for $name {
			fn partial_cmp(&self, other: &$name) -> Option<Ordering> {
				self.0.partial_cmp(&other.0)
			}
		}

		impl From<$name> for String {
			fn from(other: $name) -> Self {
				other.0.into()
			}
		}

		#[cfg(feature = "compact_str")]
		#[cfg_attr(docsrs, doc(cfg(feature = "compact_str")))]
		impl From<$name> for CompactString {
			fn from(other: $name) -> Self {
				other.0.into()
			}
		}

		impl<'x> From<$name> for Cow<'x, $borrowed> {
			fn from(other: $name) -> Self {
				Self::Owned(other)
			}
		}

		impl<'x> From<Cow<'x, $borrowed>> for $name {
			fn from(other: Cow<'x, $borrowed>) -> Self {
				other.into_owned()
			}
		}

		#[cfg(feature = "compact_str")]
		#[cfg_attr(docsrs, doc(cfg(feature = "compact_str")))]
		impl TryFrom<CompactString> for $name {
			type Error = Error;

			#[doc = rxml_safe_str_construct_doc!($name, CompactString, "")]
			fn try_from(other: CompactString) -> Result<Self, Self::Error> {
				$check(&other)?;
				Ok($name(other.into()))
			}
		}

		impl TryFrom<String> for $name {
			type Error = Error;

			#[doc = rxml_safe_str_construct_doc!($name, String, "")]
			fn try_from(other: String) -> Result<Self, Self::Error> {
				$check(&other)?;
				Ok($name(other.into()))
			}
		}

		impl TryFrom<&str> for $name {
			type Error = Error;

			#[doc = rxml_safe_str_construct_doc!($name, str, "")]
			fn try_from(other: &str) -> Result<Self, Self::Error> {
				$check(other)?;
				Ok($name(other.into()))
			}
		}

		impl fmt::Display for $name {
			fn fmt<'f>(&self, f: &'f mut fmt::Formatter) -> fmt::Result {
				f.write_str(&self.0 as &str)
			}
		}

		// The impls below here are inspired by the list of trait impls of
		// String.

		// CORRECTNESS:
		// For NcName and Name it holds that concatenations of these types are
		// correct.
		//
		// That is because while they may have constraints on the characters
		// at the beginning, the characters allowed in the remainder of the
		// respective string types are a superset of those allowed as first
		// character.
		impl Add<&$borrowed> for $name {
			type Output = $name;

			fn add(mut self, rhs: &$borrowed) -> Self::Output {
				self += rhs;
				self
			}
		}

		impl AddAssign<&$borrowed> for $name {
			fn add_assign(&mut self, rhs: &$borrowed) {
				self.0.push_str(&rhs.0)
			}
		}

		impl<'a> Extend<&'a $borrowed> for $name {
			fn extend<I: IntoIterator<Item = &'a $borrowed>>(&mut self, iter: I) {
				self.0.extend(iter.into_iter().map(|x| &x.0))
			}
		}

		impl Extend<Box<$borrowed>> for $name {
			fn extend<I: IntoIterator<Item = Box<$borrowed>>>(&mut self, iter: I) {
				for item in iter {
					self.add_assign(&item);
				}
			}
		}

		impl<'a> Extend<Cow<'a, $borrowed>> for $name {
			fn extend<I: IntoIterator<Item = Cow<'a, $borrowed>>>(&mut self, iter: I) {
				for item in iter {
					self.add_assign(&item);
				}
			}
		}

		impl Extend<$name> for $name {
			fn extend<I: IntoIterator<Item = $name>>(&mut self, iter: I) {
				self.0.extend(iter.into_iter().map(|x| x.0))
			}
		}

		impl<'x> FromIterator<&'x $borrowed> for $name {
			fn from_iter<I: IntoIterator<Item = &'x $borrowed>>(iter: I) -> Self {
				// SAFETY: see note above impl Add<&$borrowed>.
				unsafe {
					Self::from_native_unchecked(
						<$string>::from_iter(iter.into_iter().map(|x| &x.0))
					)
				}
			}
		}

		impl FromIterator<Box<$borrowed>> for $name {
			fn from_iter<I: IntoIterator<Item = Box<$borrowed>>>(iter: I) -> Self {
				let mut buf = <$string>::with_capacity(0);
				for item in iter {
					buf.push_str(&item.0);
				}
				unsafe {
					Self::from_native_unchecked(buf)
				}
			}
		}

		impl<'x> FromIterator<Cow<'x, $borrowed>> for $name {
			fn from_iter<I: IntoIterator<Item = Cow<'x, $borrowed>>>(iter: I) -> Self {
				let mut buf = <$string>::with_capacity(0);
				for item in iter {
					buf.push_str(&item.0);
				}
				unsafe {
					Self::from_native_unchecked(buf)
				}
			}
		}
	}
}

macro_rules! rxml_custom_str_type {
	(
		$(#[$outer:meta])*
		pub struct $name:ident(str) use $check:ident => $owned:ident;
	) => {
		$(#[$outer])*
		#[derive(Debug, Hash, PartialEq, Eq, Ord)]
		#[repr(transparent)]
		pub struct $name(str);

		impl $name {
			#[doc = rxml_safe_str_construct_doc!($name, str, "")]
			pub fn from_str<'x>(s: &'x str) -> Result<&'x Self, Error> {
				s.try_into()
			}

			/// Access the underlying str.
			///
			/// This is mostly provided for use in const functions.
			pub const fn as_str(&self) -> &str {
				&self.0
			}

			#[doc = rxml_unsafe_str_construct_doc!($name, str)]
			pub const unsafe fn from_str_unchecked<'x>(s: &'x str) -> &'x Self {
				core::mem::transmute(s)
			}

			#[doc = rxml_unsafe_str_construct_doc!($name, str)]
			pub unsafe fn from_str_unchecked_mut<'x>(s: &'x mut str) -> &'x mut Self {
				core::mem::transmute(s)
			}

			/// Replace A-Z with a-z, in-place.
			///
			/// Non-ASCII characters remain unchanged.
			#[doc = rxml_make_ascii_lowercase_example!($owned, $name)]
			pub fn make_ascii_lowercase(&mut self) {
				self.0.make_ascii_lowercase()
			}

			/// Replace a-z with A-Z, in-place.
			///
			/// Non-ASCII characters remain unchanged.
			#[doc = rxml_make_ascii_uppercase_example!($owned, $name)]
			pub fn make_ascii_uppercase(&mut self) {
				self.0.make_ascii_uppercase()
			}
		}

		impl Deref for $name {
			type Target = str;

			fn deref(&self) -> &Self::Target {
				&self.0
			}
		}

		impl AsRef<str> for $name {
			fn as_ref(&self) -> &str {
				&self.0
			}
		}

		impl AsRef<$name> for &$name {
			fn as_ref(&self) -> &$name {
				&self
			}
		}

		impl PartialEq<str> for $name {
			fn eq(&self, other: &str) -> bool {
				&self.0 == other
			}
		}

		impl PartialEq<$name> for str {
			fn eq(&self, other: &$name) -> bool {
				self == &other.0
			}
		}

		impl PartialOrd<$name> for $name {
			fn partial_cmp(&self, other: &$name) -> Option<Ordering> {
				self.0.partial_cmp(&other.0)
			}
		}

		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		impl ToOwned for $name {
			type Owned = $owned;

			fn to_owned(&self) ->Self::Owned {
				self.into()
			}
		}

		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		impl From<&$name> for $owned {
			fn from(other: &$name) -> Self {
				// SAFETY: $owned is assumed to use the same check; this is
				// enforced by using the pair macro.
				unsafe { $owned::from_str_unchecked(&other.0) }
			}
		}

		impl<'x> TryFrom<&'x str> for &'x $name {
			type Error = Error;

			fn try_from(other: &'x str) -> Result<Self, Self::Error> {
				$check(other)?;
				// SAFETY: the content check is executed right above and we're
				// transmuting &str into a repr(transparent) of &str.
				Ok(unsafe { core::mem::transmute(other) } )
			}
		}

		impl fmt::Display for $name {
			fn fmt<'f>(&self, f: &'f mut fmt::Formatter) -> fmt::Result {
				f.write_str(&self.0)
			}
		}
	}
}

macro_rules! rxml_index_impl {
	($ty:ty, $selcode:expr, $borrowed:ty, $rangety:ty) => {
		impl Index<$rangety> for $ty {
			type Output = $borrowed;

			fn index(&self, index: $rangety) -> &$borrowed {
				let tmp = &self.0[index];
				let firstchar = tmp.chars().next();
				if !($selcode(firstchar)) {
					panic!(concat!("slice is not a valid ", stringify!($borrowed)));
				}
				// SAFETY: please please point $selcode at the right stuff, kthxbai.
				unsafe { <$borrowed>::from_str_unchecked(tmp) }
			}
		}
	};
}

macro_rules! rxml_splitting_impls {
	($ty:ident => $firstsel:path => $borrowed:ident) => {
		rxml_splitting_impls!($ty => (|firstchar: Option<char>| firstchar.map(|x| $firstsel.select(x)).unwrap_or(false)) => $borrowed);
	};
	($ty:ident => $selcode:expr => $borrowed:ident) => {
		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		rxml_index_impl!($ty, $selcode, $borrowed, Range<usize>);

		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		rxml_index_impl!($ty, $selcode, $borrowed, RangeFrom<usize>);

		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		rxml_index_impl!($ty, $selcode, $borrowed, RangeFull);

		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		rxml_index_impl!($ty, $selcode, $borrowed, RangeInclusive<usize>);

		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		rxml_index_impl!($ty, $selcode, $borrowed, RangeTo<usize>);

		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		rxml_index_impl!($ty, $selcode, $borrowed, RangeToInclusive<usize>);

		rxml_index_impl!($borrowed, $selcode, $borrowed, Range<usize>);
		rxml_index_impl!($borrowed, $selcode, $borrowed, RangeFrom<usize>);
		rxml_index_impl!($borrowed, $selcode, $borrowed, RangeFull);
		rxml_index_impl!($borrowed, $selcode, $borrowed, RangeInclusive<usize>);
		rxml_index_impl!($borrowed, $selcode, $borrowed, RangeTo<usize>);
		rxml_index_impl!($borrowed, $selcode, $borrowed, RangeToInclusive<usize>);


		#[cfg(feature = "std")]
		#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
		impl $ty {
			/// Splits the string into two at the given byte index.
			#[doc = rxml_split_off_panics!($ty)]
			///
			/// # Example
			///
			#[doc = rxml_split_off_example!($ty)]
			pub fn split_off(&mut self, at: usize) -> Self {
				let other = self.0.split_off(at);
				if !<$borrowed>::verify(&other) || !<$borrowed>::verify(&self.0) {
					panic!(concat!("split string is not a valid ", stringify!($ty)));
				}
				// SAFETY: please please point $selcode at the right stuff, kthxbai.
				unsafe {
					<$ty>::from_str_unchecked(other)
				}
			}
		}

		impl $borrowed {
			fn verify(s: &str) -> bool {
				let firstchar = s.chars().next();
				return $selcode(firstchar);
			}

			/// Divide one string slice into two at an index.
			///
			/// # Panics
			///
			/// Panics if `mid` is not on a UTF-8 code point boundary, or if
			/// it is past the end of the last code point of the slice, or
			/// if either resulting part is not a valid slice of this type.
			///
			/// # Example
			#[doc = rxml_split_at_example!($borrowed)]
			pub fn split_at(&self, mid: usize) -> (&Self, &Self) {
				let (a, b) = self.0.split_at(mid);
				if !Self::verify(a) || !Self::verify(b) {
					panic!(concat!("split_at result is not a valid ", stringify!($borrowed)));
				}
				// SAFETY: please please point $selcode at the right stuff, kthxbai.
				unsafe {
					(
						Self::from_str_unchecked(a),
						Self::from_str_unchecked(b),
					)
				}
			}

			/// Divide one mutable string slice into two at an index.
			///
			/// # Panics
			///
			/// Panics if `mid` is not on a UTF-8 code point boundary, or if
			/// it is past the end of the last code point of the slice, or
			/// if either resulting part is not a valid slice of this type.
			pub fn split_at_mut(&mut self, mid: usize) -> (&mut Self, &mut Self) {
				let (a, b) = self.0.split_at_mut(mid);
				if !Self::verify(a) || !Self::verify(b) {
					panic!(concat!("split_at_mut result is not a valid ", stringify!($borrowed)));
				}
				// SAFETY: please please point $selcode at the right stuff, kthxbai.
				unsafe {
					(
						Self::from_str_unchecked_mut(a),
						Self::from_str_unchecked_mut(b),
					)
				}
			}
		}
	}
}

macro_rules! rxml_custom_string_type_pair {
	(
		$(#[$ownedmeta:meta])*
		pub struct $owned:ident($string:ident) use $check:ident;

		$(#[$borrowedmeta:meta])*
		pub struct $borrowed:ident(str);
	) => {
		#[cfg(feature = "std")]
		rxml_custom_string_type!{
			$(#[$ownedmeta])*
			#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
			pub struct $owned($string) use $check => $borrowed;
		}

		rxml_custom_str_type!{
			$(#[$borrowedmeta])*
			pub struct $borrowed(str) use $check => $owned;
		}
	}
}

rxml_custom_string_type_pair! {
	/// String which conforms to the Name production of XML 1.0.
	///
	/// [`Name`] corresponds to a (restricted) [`String`]. For a [`str`]-like type
	/// with the same restrictions, see [`NameStr`].
	///
	/// If using the `rxml` crate and the `macros` feature of the `rxml` crate
	/// is enabled, `&NameStr` can be created from a string literal at compile
	/// time using the `rxml::xml_name` macro.
	///
	/// Since [`Name`] (indirectly) derefs to [`str`], all (non-mutable)
	/// methods from [`str`] are available.
	///
	/// # Formal definition
	///
	/// The data inside [`Name`] (and [`NameStr`]) is guaranteed to conform to
	/// the `Name` production of the below grammar, quoted from
	/// [XML 1.0 § 2.3](https://www.w3.org/TR/REC-xml/#NT-NameStartChar):
	///
	/// ```text
	/// [4]  NameStartChar ::= ":" | [A-Z] | "_" | [a-z] | [#xC0-#xD6]
	///                        | [#xD8-#xF6] | [#xF8-#x2FF] | [#x370-#x37D]
	///                        | [#x37F-#x1FFF] | [#x200C-#x200D]
	///                        | [#x2070-#x218F] | [#x2C00-#x2FEF]
	///                        | [#x3001-#xD7FF] | [#xF900-#xFDCF]
	///                        | [#xFDF0-#xFFFD] | [#x10000-#xEFFFF]
	/// [4a] NameChar      ::= NameStartChar | "-" | "." | [0-9] | #xB7
	///                        | [#x0300-#x036F] | [#x203F-#x2040]
	/// [5]  Name          ::= NameStartChar (NameChar)*
	/// ```
	pub struct Name(CompactString) use validate_name;

	/// str which conforms to the Name production of XML 1.0.
	///
	/// [`NameStr`] corresponds to a (restricted) [`str`]. For a [`String`]-like
	/// type with the same restrictions as well as the formal definition of those
	/// restrictions, see [`Name`].
	///
	/// If using the `rxml` crate and the `macros` feature of the `rxml` crate
	/// is enabled, `&NameStr` can be created from a string literal at compile
	/// time using the `rxml::xml_name` macro.
	///
	/// Since [`NameStr`] derefs to [`str`], all (non-mutable) methods from
	/// [`str`] are available.
	pub struct NameStr(str);
}

rxml_splitting_impls! {
	Name => selectors::CLASS_XML_NAMESTART => NameStr
}

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl Name {
	/// Split the name at a colon, if it exists.
	///
	/// If the name contains no colon, the function returns `(None, self)`.
	/// If the name contains exactly one colon, the function returns the part
	/// before the colon (the prefix) in the first return value and the part
	/// following the colon (the suffix) as second return value.
	///
	/// If neither of the two cases apply or the string on either side of the
	/// colon is empty, an error is returned.
	///
	/// This function optimizes the split (compared to operating on a borrowed
	/// [`NameStr`] and then cloning the returned parts) by avoiding
	/// unnecessary copying.
	///
	/// # Example
	///
	/// ```
	/// # use rxml_validation::Name;
	/// # use std::convert::TryFrom;
	/// // with prefix:
	/// let name = Name::try_from("foo:bar").unwrap();
	/// let (prefix, local) = name.split_name().unwrap();
	/// assert_eq!(prefix.unwrap(), "foo");
	/// assert_eq!(local, "bar");
	///
	/// // without prefix:
	/// let name = Name::try_from("no-prefix").unwrap();
	/// let (prefix, local) = name.split_name().unwrap();
	/// assert!(prefix.is_none());
	/// assert_eq!(local, "no-prefix");
	/// ```
	pub fn split_name(self) -> Result<(Option<NcName>, NcName), Error> {
		let mut name = self.0;
		let colon_pos = match name.find(':') {
			None => return Ok((None, unsafe { NcName::from_auto_unchecked(name) })),
			Some(pos) => pos,
		};
		if colon_pos == 0 || colon_pos == name.len() - 1 {
			return Err(Error::EmptyNamePart);
		}

		let localname = name.split_off(colon_pos + 1);
		let mut prefix = name;

		if localname.find(':').is_some() {
			// Namespaces in XML 1.0 (Third Edition) namespace-well-formed criterium 1
			return Err(Error::MultiColonName);
		};
		if !selectors::CLASS_XML_NAMESTART.select(localname.chars().next().unwrap()) {
			// Namespaces in XML 1.0 (Third Edition) NcName production
			return Err(Error::InvalidLocalName);
		}

		prefix.pop();
		// do not shrink to fit here -- the prefix will be used when the element
		// is finalized to put it on the stack for quick validation of the
		// </element> token.

		debug_assert!(prefix.len() > 0);
		debug_assert!(localname.len() > 0);
		Ok((
			Some(unsafe { NcName::from_auto_unchecked(prefix) }),
			unsafe { NcName::from_auto_unchecked(localname) },
		))
	}
}

impl NameStr {
	/// Create an owned copy of the string as [`Name`].
	///
	/// This operation is also available as implementation of the `Into`
	/// trait.
	#[cfg(feature = "std")]
	#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
	pub fn to_name(&self) -> Name {
		self.into()
	}

	/// Split the name at a colon, if it exists.
	///
	/// If the name contains no colon, the function returns `(None, self)`.
	/// If the name contains exactly one colon, the function returns the part
	/// before the colon (the prefix) in the first return value and the part
	/// following the colon (the suffix) as second return value.
	///
	/// If neither of the two cases apply or the string on either side of the
	/// colon is empty, an error is returned.
	///
	/// # Example
	///
	/// ```
	/// # use rxml_validation::NameStr;
	/// # use std::convert::TryInto;
	/// // with prefix:
	/// let name: &NameStr = "foo:bar".try_into().unwrap();
	/// let (prefix, local) = name.split_name().unwrap();
	/// assert_eq!(prefix.unwrap(), "foo");
	/// assert_eq!(local, "bar");
	///
	/// // without prefix:
	/// let name: &NameStr = "no-prefix".try_into().unwrap();
	/// let (prefix, local) = name.split_name().unwrap();
	/// assert!(prefix.is_none());
	/// assert_eq!(local, "no-prefix");
	/// ```
	pub fn split_name(&self) -> Result<(Option<&'_ NcNameStr>, &'_ NcNameStr), Error> {
		let name = &self.0;
		let colon_pos = match name.find(':') {
			None => return Ok((None, unsafe { NcNameStr::from_str_unchecked(name) })),
			Some(pos) => pos,
		};
		if colon_pos == 0 || colon_pos == name.len() - 1 {
			return Err(Error::EmptyNamePart);
		}

		let (prefix, localname) = name.split_at(colon_pos);
		let localname = &localname[1..];

		if localname.find(':').is_some() {
			// Namespaces in XML 1.0 (Third Edition) namespace-well-formed criterium 1
			return Err(Error::MultiColonName);
		};
		if !selectors::CLASS_XML_NAMESTART.select(localname.chars().next().unwrap()) {
			// Namespaces in XML 1.0 (Third Edition) NcName production
			return Err(Error::InvalidLocalName);
		}

		debug_assert!(prefix.len() > 0);
		debug_assert!(localname.len() > 0);
		Ok((
			Some(unsafe { NcNameStr::from_str_unchecked(prefix) }),
			unsafe { NcNameStr::from_str_unchecked(localname) },
		))
	}
}

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl From<NcName> for Name {
	fn from(other: NcName) -> Self {
		other.into_name()
	}
}

impl<'x> From<&'x NcNameStr> for &'x NameStr {
	fn from(other: &'x NcNameStr) -> Self {
		other.as_namestr()
	}
}

rxml_custom_string_type_pair! {
	/// String which conforms to the NcName production of Namespaces in XML 1.0.
	///
	/// [`NcName`] corresponds to a (restricted) [`String`]. For a [`str`]-like
	/// type with the same restrictions, see [`NcNameStr`].
	///
	/// If using the `rxml` crate and the `macros` feature of the `rxml` crate
	/// is enabled, `&NcNameStr` can be created from a string literal at
	/// compile time using the `rxml::xml_ncname` macro.
	///
	/// Since [`NcName`] (indirectly) derefs to [`str`], all (non-mutable)
	/// methods from [`str`] are available.
	///
	/// # Formal definition
	///
	/// The data inside [`NcName`] (and [`NcNameStr`]) is guaranteed to conform to
	/// the `NcName` production of the below grammar, quoted from
	/// [Namespaces in XML 1.0 § 3](https://www.w3.org/TR/REC-xml-names/#NT-NcName):
	///
	/// ```text
	/// [4] NcName ::= Name - (Char* ':' Char*)  /* An XML Name, minus the ":" */
	/// ```
	pub struct NcName(CompactString) use validate_ncname;

	/// str which conforms to the NcName production of Namespaces in XML 1.0.
	///
	/// [`NcNameStr`] corresponds to a (restricted) [`str`]. For a [`String`]-like
	/// type with the same restrictions as well as the formal definition of those
	/// restrictions, see [`NcName`].
	///
	/// If using the `rxml` crate and the `macros` feature of the `rxml` crate
	/// is enabled, `&NcNameStr` can be created from a string literal at
	/// compile time using the `rxml::xml_ncname` macro.
	///
	/// Since [`NcNameStr`] derefs to [`str`], all (non-mutable) methods from
	/// [`str`] are available.
	pub struct NcNameStr(str);
}

rxml_splitting_impls! {
	NcName => selectors::CLASS_XML_NAMESTART => NcNameStr
}

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl NcName {
	/// Compose two [`NcName`] objects to one [`Name`], separating them with
	/// a colon.
	///
	/// As an [`NcName`] is always a valid [`Name`], the composition of the
	/// two with a `:` as separator is also a valid [`Name`].
	///
	/// This is the inverse of [`Name::split_name()`].
	///
	/// # Example
	///
	/// ```
	/// # use rxml_validation::NcName;
	/// # use std::convert::TryFrom;
	/// let prefix = NcName::try_from("xmlns").unwrap();
	/// let localname = NcName::try_from("stream").unwrap();
	/// assert_eq!(prefix.add_suffix(&localname), "xmlns:stream");
	/// ```
	pub fn add_suffix(self, suffix: &NcNameStr) -> Name {
		let mut s: String = self.0.into();
		s.reserve(suffix.len() + 1);
		s.push_str(":");
		s.push_str(suffix);
		// SAFETY: NcName cannot contain a colon; Name is NcName with colons,
		// so we can concat two NcNames to a Name.
		unsafe { Name::from_string_unchecked(s) }
	}

	/// Convert the [`NcName`] into a [`Name`].
	///
	/// This operation is O(1).
	///
	/// This operation is also available as implementation of the `Into`
	/// trait.
	pub fn into_name(self) -> Name {
		// SAFETY: NcName is a strict subset of Name
		unsafe { Name::from_auto_unchecked(self.0) }
	}
}

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl AsRef<NameStr> for NcName {
	fn as_ref(&self) -> &NameStr {
		<Self as AsRef<NcNameStr>>::as_ref(self).as_ref()
	}
}

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl Borrow<NameStr> for NcName {
	fn borrow(&self) -> &NameStr {
		<Self as Borrow<NcNameStr>>::borrow(self).borrow()
	}
}

impl NcNameStr {
	/// Create an owned copy of the string as [`NcName`].
	///
	/// This operation is also available as implementation of the `Into`
	/// trait.
	#[cfg(feature = "std")]
	#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
	pub fn to_ncname(&self) -> NcName {
		self.into()
	}

	/// Create an owned copy of the string as [`Name`].
	#[cfg(feature = "std")]
	#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
	pub fn to_name(&self) -> Name {
		self.to_ncname().into()
	}

	/// Access the string as [`NameStr`].
	///
	/// This operation is O(1), as Names are a strict superset of NcNames.
	pub fn as_namestr<'x>(&'x self) -> &'x NameStr {
		// SAFETY: NcName is a strict subset of Name
		unsafe { NameStr::from_str_unchecked(&self.0) }
	}

	/// Compose two [`NcName`] objects to one [`Name`], separating them with
	/// a colon.
	///
	/// As an [`NcName`] is always a valid [`Name`], the composition of the
	/// two with a `:` as separator is also a valid [`Name`].
	///
	/// This is the inverse of [`Name::split_name()`].
	///
	/// # Example
	///
	/// ```
	/// # use rxml_validation::NcName;
	/// # use std::convert::TryFrom;
	/// let prefix = NcName::try_from("xmlns").unwrap();
	/// let localname = NcName::try_from("stream").unwrap();
	/// assert_eq!(prefix.add_suffix(&localname), "xmlns:stream");
	/// ```
	#[cfg(feature = "std")]
	#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
	pub fn with_suffix(&self, suffix: &NcNameStr) -> Name {
		let mut s = String::with_capacity(self.len() + 1 + suffix.len());
		s.push_str(self);
		s.push_str(":");
		s.push_str(suffix);
		// SAFETY: NcName cannot contain a colon; Name is NcName with colons,
		// so we can concat two NcNames to a Name.
		unsafe { Name::from_string_unchecked(s) }
	}
}

impl AsRef<NameStr> for NcNameStr {
	fn as_ref(&self) -> &NameStr {
		// SAFETY: NameStr rules are a superset of the NcNameStr rules.
		unsafe { NameStr::from_str_unchecked(&self.0) }
	}
}

impl Borrow<NameStr> for NcNameStr {
	fn borrow(&self) -> &NameStr {
		self.as_ref()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn split_name_on_namestr_with_valid_name() {
		let nm: &NameStr = "foo:bar".try_into().unwrap();
		let (prefix, localname) = nm.split_name().unwrap();
		assert_eq!(prefix.unwrap(), "foo");
		assert_eq!(localname, "bar");
	}

	#[test]
	fn split_name_on_namestr_with_prefixless_name() {
		let nm: &NameStr = "bar".try_into().unwrap();
		let (prefix, localname) = nm.split_name().unwrap();
		assert_eq!(prefix, None);
		assert_eq!(localname, "bar");
	}

	#[test]
	fn split_name_on_namestr_rejects_localname_with_non_namestart_first_char() {
		let nm: &NameStr = "foo:-bar".try_into().unwrap();
		let result = nm.split_name();
		assert!(matches!(result.err().unwrap(), Error::InvalidLocalName,));
	}

	#[test]
	#[cfg(feature = "std")]
	fn split_name_on_name_with_valid_name() {
		let nm: Name = "foo:bar".try_into().unwrap();
		let (prefix, localname) = nm.split_name().unwrap();
		assert_eq!(prefix.unwrap(), "foo");
		assert_eq!(localname, "bar");
	}

	#[test]
	#[cfg(feature = "std")]
	fn split_name_on_name_with_prefixless_name() {
		let nm: Name = "bar".try_into().unwrap();
		let (prefix, localname) = nm.split_name().unwrap();
		assert_eq!(prefix, None);
		assert_eq!(localname, "bar");
	}

	#[test]
	#[cfg(feature = "std")]
	fn split_name_on_name_rejects_localname_with_non_namestart_first_char() {
		let nm: Name = "foo:-bar".try_into().unwrap();
		let result = nm.split_name();
		assert!(matches!(result.err().unwrap(), Error::InvalidLocalName,));
	}

	#[test]
	fn split_namestr_on_name_with_valid_name() {
		let nm: &NameStr = "foo:bar".try_into().unwrap();
		let (prefix, localname) = nm.split_name().unwrap();
		assert_eq!(prefix.unwrap(), "foo");
		assert_eq!(localname, "bar");
	}

	#[test]
	fn split_namestr_on_name_with_prefixless_name() {
		let nm: &NameStr = "bar".try_into().unwrap();
		let (prefix, localname) = nm.split_name().unwrap();
		assert_eq!(prefix, None);
		assert_eq!(localname, "bar");
	}

	#[test]
	fn split_namestr_on_name_rejects_localname_with_non_namestart_first_char() {
		let nm: &NameStr = "foo:-bar".try_into().unwrap();
		let result = nm.split_name();
		assert!(matches!(result.err().unwrap(), Error::InvalidLocalName,));
	}

	#[test]
	#[should_panic(expected = "slice is not a valid NameStr")]
	fn namestr_slice_panics_on_non_name_start() {
		let x: &NameStr = "foo-bar".try_into().unwrap();
		let _: &NameStr = &x[3..];
	}

	#[test]
	#[should_panic(expected = "slice is not a valid NameStr")]
	#[cfg(feature = "std")]
	fn name_slice_panics_on_non_name_start() {
		let x: Name = "foo-bar".try_into().unwrap();
		let _: &NameStr = &x[3..];
	}

	#[test]
	#[should_panic(expected = "split string is not a valid Name")]
	#[cfg(feature = "std")]
	fn name_split_off_refuses_empty_lhs() {
		let mut x: Name = "foobar".try_into().unwrap();
		x.split_off(0);
	}

	#[test]
	#[should_panic(expected = "split string is not a valid Name")]
	#[cfg(feature = "std")]
	fn name_split_off_refuses_empty_rhs() {
		let mut x: Name = "foobar".try_into().unwrap();
		x.split_off(6);
	}

	#[test]
	#[should_panic(expected = "slice is not a valid NcNameStr")]
	fn ncnamestr_slice_panics_on_non_name_start() {
		let x: &NcNameStr = "foo-bar".try_into().unwrap();
		let _: &NcNameStr = &x[3..];
	}

	#[test]
	#[should_panic(expected = "slice is not a valid NcNameStr")]
	#[cfg(feature = "std")]
	fn ncname_slice_panics_on_non_name_start() {
		let x: NcName = "foo-bar".try_into().unwrap();
		let _: &NcNameStr = &x[3..];
	}

	#[test]
	fn ncname_refuses_empty_slice() {
		match <&str as TryInto<&NcNameStr>>::try_into("") {
			Err(_) => (),
			other => panic!("unexpected result: {:?}", other),
		}
	}
}
