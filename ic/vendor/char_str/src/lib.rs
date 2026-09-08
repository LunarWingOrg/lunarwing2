#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

use core::{
    borrow::Borrow,
    cmp, fmt,
    hash::{Hash, Hasher},
    ops::{Add, AddAssign, Deref},
    str,
    str::FromStr,
};

use alloc::{borrow::Cow, boxed::Box, string::String};

#[cfg(feature = "std")]
use std::ffi::OsStr;

mod repr;
use repr::Repr;

mod char_str;
pub use char_str::CharStr;

mod errors;
pub use errors::*;

mod traits;
pub use traits::ToCharString;

mod features;

/// Formats text into a [`CharString`].
///
/// This is equivalent to [`format!`](alloc::format), but writes directly into a compact,
/// growable string.
///
/// # Panics
///
/// Panics if a formatting trait implementation returns an error.
///
/// # Examples
///
/// ```
/// # use char_str::format_char;
/// let name = "world";
/// assert_eq!(format_char!("hello, {name}!"), "hello, world!");
/// ```
#[macro_export]
macro_rules! format_char {
    ($($arg:tt)*) => {{
        let mut string = $crate::CharString::new();
        ::core::fmt::Write::write_fmt(&mut string, ::core::format_args!($($arg)*))
            .expect("a formatting trait implementation returned an error");
        string
    }};
}

/// Formats text into an immutable [`CharStr`].
///
/// # Panics
///
/// Panics if a formatting trait implementation returns an error or freezing the formatted string
/// fails.
///
/// # Examples
///
/// ```
/// # use char_str::format_char_str;
/// let package = "package";
/// let module = "module";
/// assert_eq!(format_char_str!("{package}.{module}"), "package.module");
/// ```
#[macro_export]
macro_rules! format_char_str {
    ($($arg:tt)*) => {
        $crate::format_char!($($arg)*).freeze()
    };
}

/// Compact, clone-on-write, UTF-8 encoded, growable string type.
///
/// Heap allocations store a capacity and are always growable. Use [`CharString::freeze`] to
/// convert to the capacity-less heap representation used by [`CharStr`].
#[repr(transparent)]
pub struct CharString(Repr);

const _: () = {
    assert!(size_of::<CharString>() == size_of::<[usize; 2]>());
    assert!(size_of::<Option<CharString>>() == size_of::<[usize; 2]>());
    assert!(align_of::<CharString>() == align_of::<usize>());
    assert!(align_of::<Option<CharString>>() == align_of::<usize>());
};

impl CharString {
    pub(crate) fn from_repr(repr: Repr) -> Self {
        debug_assert!(!repr.is_exact_heap_buffer());
        Self(repr)
    }

    /// Creates a new empty [`CharString`].
    ///
    /// Same as [`String::new()`], this will not allocate on the heap.
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::new();
    /// assert!(s.is_empty());
    /// assert!(!s.is_heap_allocated());
    /// ```
    #[inline]
    pub const fn new() -> Self {
        CharString(Repr::new())
    }

    /// Converts this value into an immutable, exactly allocated [`CharStr`].
    ///
    /// Unique growable heap storage is converted to exact storage with `realloc`. Shared heap
    /// storage is copied into a new exact allocation.
    ///
    /// # Panics
    ///
    /// Panics if converting heap storage fails. To handle allocation failure, use
    /// [`CharString::try_freeze()`].
    #[inline]
    pub fn freeze(self) -> CharStr {
        self.try_freeze().unwrap_with_msg()
    }

    /// Tries to convert this value into an immutable, exactly allocated [`CharStr`].
    ///
    /// Inline and static storage are transferred without reallocating. Unique growable heap
    /// storage is converted with `realloc`, while shared heap storage is copied.
    ///
    /// # Errors
    ///
    /// Returns a [`ReserveError`] if converting heap storage fails. Because this method consumes
    /// `self`, the original [`CharString`] is dropped on failure.
    #[inline]
    pub fn try_freeze(mut self) -> Result<CharStr, ReserveError> {
        self.0.make_exact()?;
        Ok(CharStr::from_repr(core::mem::replace(&mut self.0, Repr::new())))
    }

    /// Creates a new [`CharString`] from a `&'static str`.
    ///
    /// # Panics
    ///
    /// Panics if `text` is longer than `2^56 - 1` bytes on a 64-bit architecture or
    /// `2^24 - 1` (`16_777_215`) bytes on a 32-bit architecture.
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::from_static_str("Long text but static lifetime");
    /// assert_eq!(s.as_str(), "Long text but static lifetime");
    /// assert_eq!(s.len(), 29);
    /// assert!(!s.is_heap_allocated());
    /// ```
    #[inline]
    pub const fn from_static_str(text: &'static str) -> Self {
        match Repr::from_static_str(text) {
            Ok(repr) => CharString(repr),
            Err(_) => panic!("text is too long"),
        }
    }

    /// Creates a new empty [`CharString`] with at least capacity bytes.
    ///
    /// A [`CharString`] will inline strings if the length is less than or equal to
    /// `2 * size_of::<usize>()` bytes. This means that the minimum capacity of a [`CharString`]
    /// is `2 * size_of::<usize>()` bytes.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions is met:
    ///
    /// - The system is out-of-memory.
    /// - On 64-bit architecture, the `capacity` is greater than `2^56 - 1`.
    /// - On 32-bit architecture, the `capacity` is greater than `2^31 - 16` (`2_147_483_632`).
    ///
    /// If you want to handle such a problem manually, use [`CharString::try_with_capacity()`].
    ///
    /// # Examples
    ///
    /// ## inline capacity
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::with_capacity(4);
    /// assert_eq!(s.capacity(), 2 * size_of::<usize>());
    /// assert!(!s.is_heap_allocated());
    /// ```
    ///
    /// ## heap capacity
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::with_capacity(100);
    /// assert_eq!(s.capacity(), 100);
    /// assert!(s.is_heap_allocated());
    /// ```
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        CharString::try_with_capacity(capacity).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::with_capacity()`].
    ///
    /// This method won't panic if the system is out of memory, or if the `capacity` is too large, but
    /// returns a [`ReserveError`]. Otherwise it behaves the same as [`CharString::with_capacity()`].
    #[inline]
    pub fn try_with_capacity(capacity: usize) -> Result<Self, ReserveError> {
        Repr::with_capacity(capacity).map(CharString)
    }

    /// Converts a slice of bytes to a [`CharString`].
    ///
    /// If the slice is not valid UTF-8, an error is returned.
    ///
    /// # Examples
    ///
    /// ## valid UTF-8
    ///
    /// ```
    /// # use char_str::CharString;
    /// let bytes = vec![240, 159, 166, 128];
    /// let string = CharString::from_utf8(&bytes).expect("valid UTF-8");
    ///
    /// assert_eq!(string, "🦀");
    /// ```
    ///
    /// ## invalid UTF-8
    ///
    /// ```
    /// # use char_str::CharString;
    /// let bytes = &[255, 255, 255];
    /// let result = CharString::from_utf8(bytes);
    ///
    /// assert!(result.is_err());
    /// ```
    #[inline]
    pub fn from_utf8(buf: &[u8]) -> Result<Self, str::Utf8Error> {
        let str = str::from_utf8(buf)?;
        Ok(CharString::from(str))
    }

    /// Converts a slice of bytes to a [`CharString`], including invalid characters.
    ///
    /// During this conversion, all invalid characters are replaced with the
    /// [`char::REPLACEMENT_CHARACTER`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let invalid_bytes = b"Hello \xF0\x90\x80World";
    /// let string = CharString::from_utf8_lossy(invalid_bytes);
    ///
    /// assert_eq!(string, "Hello �World");
    /// ```
    #[inline]
    pub fn from_utf8_lossy(buf: &[u8]) -> Self {
        let mut ret = CharString::with_capacity(buf.len());
        for chunk in buf.utf8_chunks() {
            ret.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                ret.push(char::REPLACEMENT_CHARACTER);
            }
        }
        ret
    }

    /// Converts a slice of bytes to a [`CharString`] without checking if the bytes are valid
    /// UTF-8.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the bytes passed to it are valid
    /// UTF-8. If this constraint is violated, it may cause memory unsafety issues.
    #[inline]
    pub unsafe fn from_utf8_unchecked(buf: &[u8]) -> Self {
        let str = unsafe { str::from_utf8_unchecked(buf) };
        CharString::from(str)
    }

    /// Decodes a slice of UTF-16 encoded bytes to a [`CharString`], returning an error if `buf`
    /// contains any invalid code points.
    ///
    /// # Examples
    ///
    /// ## valid UTF-16
    ///
    /// ```
    /// # use char_str::CharString;
    /// let v = &[0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0x0069, 0x0063];
    /// assert_eq!(CharString::from_utf16(v).unwrap(), "𝄞music");
    /// ```
    ///
    /// ## invalid UTF-16
    ///
    /// ```
    /// # use char_str::CharString;
    /// // 𝄞mu<invalid>ic
    /// let v = &[0xD834, 0xDD1E, 0x006d, 0x0075, 0xD800, 0x0069, 0x0063];
    /// assert!(CharString::from_utf16(v).is_err());
    /// ```
    #[inline]
    pub fn from_utf16(buf: &[u16]) -> Result<Self, FromUtf16Error> {
        let mut ret = CharString::with_capacity(buf.len());
        for c in char::decode_utf16(buf.iter().copied()) {
            match c {
                Ok(c) => ret.push(c),
                Err(_) => return Err(FromUtf16Error),
            }
        }
        Ok(ret)
    }

    /// Decodes a slice of UTF-16 encoded bytes to a [`CharString`], replacing invalid code points
    /// with the [`char::REPLACEMENT_CHARACTER`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// // 𝄞mus<invalid>ic<invalid>
    /// let v = &[0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834];
    /// assert_eq!(CharString::from_utf16_lossy(v), "𝄞mus\u{FFFD}ic\u{FFFD}");
    /// ```
    #[inline]
    pub fn from_utf16_lossy(buf: &[u16]) -> Self {
        char::decode_utf16(buf.iter().copied())
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }

    /// Returns the length of the string in bytes, not [`char`] or graphemes.
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let a = CharString::from("foo");
    /// assert_eq!(a.len(), 3);
    ///
    /// let fancy_f = CharString::from("ƒoo");
    /// assert_eq!(fancy_f.len(), 4);
    /// assert_eq!(fancy_f.chars().count(), 3);
    /// ```
    #[inline]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the [`CharString`] has a length of 0, `false` otherwise
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::new();
    /// assert!(s.is_empty());
    ///
    /// s.push('a');
    /// assert!(!s.is_empty());
    /// ```
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the capacity of the [`CharString`], in bytes.
    ///
    /// Inline values have a capacity of `2 * size_of::<usize>()` bytes. Heap values return their
    /// stored growable capacity. A heap-allocated [`CharStr`] remains heap-allocated when converted
    /// to growable storage, so the resulting capacity can be smaller than the inline capacity.
    ///
    /// # Examples
    ///
    /// ## inline capacity
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::new();
    /// assert_eq!(s.capacity(), 2 * size_of::<usize>());
    /// ```
    ///
    /// ## heap capacity
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::with_capacity(100);
    /// assert_eq!(s.capacity(), 100);
    /// ```
    #[inline]
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }

    /// Returns a string slice containing the entire [`CharString`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::from("foo");
    /// assert_eq!(s.as_str(), "foo");
    /// ```
    #[inline]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns a byte slice containing the entire [`CharString`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::from("hello");
    /// assert_eq!(&[104, 101, 108, 108, 111], s.as_bytes());
    /// ```
    #[inline]
    pub const fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Reserves capacity for at least `additional` bytes more than the current length.
    ///
    /// # Note
    ///
    /// This method clones the [`CharString`] if it is not unique.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions is met:
    ///
    /// - The system is out-of-memory.
    /// - On 64-bit architecture, the `capacity` is greater than `2^56 - 1`.
    /// - On 32-bit architecture, the `capacity` is greater than `2^31 - 16` (`2_147_483_632`).
    ///
    /// If you want to handle such a problem manually, use [`CharString::try_reserve()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::new();
    ///
    /// // We have an inline storage on the stack.
    /// assert_eq!(s.capacity(), 2 * size_of::<usize>());
    /// assert!(!s.is_heap_allocated());
    ///
    /// s.reserve(100);
    ///
    /// // Now we have a heap storage.
    /// assert!(s.capacity() >= s.len() + 100);
    /// assert!(s.is_heap_allocated());
    /// ```
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.try_reserve(additional).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::reserve()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` is too large, but
    /// return an [`ReserveError`]. Otherwise it behaves the same as [`CharString::reserve()`].
    #[inline]
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), ReserveError> {
        self.0.reserve(additional)
    }

    /// Shrinks the capacity of the [`CharString`] to match its length.
    ///
    /// The resulting capacity is always greater than `2 * size_of::<usize>()` bytes because
    /// [`CharString`] has inline (on the stack) storage.
    ///
    /// # Note
    ///
    /// This method clones the [`CharString`] if it is not unique and its capacity is greater than
    /// its length.
    ///
    /// # Panics
    ///
    /// Panics if cloning the [`CharString`] fails due to the system being out-of-memory. If you
    /// want to handle such a problem manually, use [`CharString::try_shrink_to_fit()`].
    ///
    /// # Examples
    ///
    /// ## short string
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("foo");
    ///
    /// s.reserve(100);
    /// assert_eq!(s.capacity(), 3 + 100);
    ///
    /// s.shrink_to_fit();
    /// assert_eq!(s.capacity(), 2 * size_of::<usize>());
    /// ```
    ///
    /// ## long string
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("This is a text the length is more than 16 bytes");
    ///
    /// s.reserve(100);
    /// assert!(s.capacity() > 16 + 100);
    ///
    /// s.shrink_to_fit();
    /// assert_eq!(s.capacity(), s.len());
    /// ```
    #[inline]
    pub fn shrink_to_fit(&mut self) {
        self.try_shrink_to_fit().unwrap_with_msg()
    }

    /// Fallible version of [`CharString::shrink_to_fit()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` is too large, but
    /// return an [`ReserveError`]. Otherwise it behaves the same as [`CharString::shrink_to_fit()`].
    #[inline]
    pub fn try_shrink_to_fit(&mut self) -> Result<(), ReserveError> {
        self.0.shrink_to(0)
    }

    /// Shrinks the capacity of the [`CharString`] with a lower bound.
    ///
    /// The resulting capacity is always greater than `2 * size_of::<usize>()` bytes because the
    /// [`CharString`] has inline (on the stack) storage.
    ///
    /// # Note
    ///
    /// This method clones the [`CharString`] if it is not unique and its capacity will be changed.
    ///
    /// # Panics
    ///
    /// Panics if cloning the [`CharString`] fails due to the system being out-of-memory. If you
    /// want to handle such a problem manually, use [`CharString::try_shrink_to()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::with_capacity(100);
    /// assert_eq!(s.capacity(), 100);
    ///
    /// // if the capacity was already bigger than the argument and unique, the call is no-op.
    /// s.shrink_to(100);
    /// assert_eq!(s.capacity(), 100);
    ///
    /// s.shrink_to(50);
    /// assert_eq!(s.capacity(), 50);
    ///
    /// // if the string can be inlined, it is
    /// s.shrink_to(5);
    /// assert_eq!(s.capacity(), 2 * size_of::<usize>());
    /// ```
    #[inline]
    pub fn shrink_to(&mut self, min_capacity: usize) {
        self.try_shrink_to(min_capacity).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::shrink_to()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` is too large, but
    /// return an [`ReserveError`]. Otherwise it behaves the same as [`CharString::shrink_to()`].
    #[inline]
    pub fn try_shrink_to(&mut self, min_capacity: usize) -> Result<(), ReserveError> {
        self.0.shrink_to(min_capacity)
    }

    /// Appends the given [`char`] to the end of the [`CharString`].
    ///
    /// # Panics
    ///
    /// Panics if the system is out-of-memory. If you want to handle such a problem manually, use
    /// [`CharString::try_push()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::new();
    /// s.push('f');
    /// s.push('o');
    /// s.push('o');
    /// assert_eq!("foo", s);
    /// ```
    #[inline]
    pub fn push(&mut self, ch: char) {
        self.try_push(ch).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::push()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` is too large, but
    /// return an [`ReserveError`]. Otherwise it behaves the same as [`CharString::push()`].
    #[inline]
    pub fn try_push(&mut self, ch: char) -> Result<(), ReserveError> {
        self.0.push_str(ch.encode_utf8(&mut [0; 4]))
    }

    /// Removes the last character from the [`CharString`] and returns it.
    /// If the [`CharString`] is empty, `None` is returned.
    ///
    /// # Panics
    ///
    /// On 32-bit architectures, this method needs to clone the [`CharString`] under both of the
    /// following conditions, and may panic if that allocation fails:
    ///
    /// - The [`CharString`] is not unique.
    /// - Its length is greater than `2^24 - 2`.
    ///
    /// If you want to handle such a problem manually, use [`CharString::try_pop()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("abč");
    ///
    /// assert_eq!(s.pop(), Some('č'));
    /// assert_eq!(s.pop(), Some('b'));
    /// assert_eq!(s.pop(), Some('a'));
    ///
    /// assert_eq!(s.pop(), None);
    /// ```
    #[inline]
    pub fn pop(&mut self) -> Option<char> {
        self.try_pop().unwrap_with_msg()
    }

    /// Fallible version of [`CharString::pop()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` is too large, but
    /// return an [`ReserveError`]. Otherwise it behaves the same as [`CharString::pop()`].
    #[inline]
    pub fn try_pop(&mut self) -> Result<Option<char>, ReserveError> {
        self.0.pop()
    }

    /// Appends a given string slice onto the end of this [`CharString`].
    ///
    /// # Panics
    ///
    /// Panics if cloning the [`CharString`] fails due to the system being out-of-memory. If you
    /// want to handle such a problem manually, use [`CharString::try_push_str()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("foo");
    ///
    /// s.push_str("bar");
    ///
    /// assert_eq!("foobar", s);
    /// ```
    #[inline]
    pub fn push_str(&mut self, string: &str) {
        self.try_push_str(string).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::push_str()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` is too large, but
    /// return an [`ReserveError`]. Otherwise it behaves the same as [`CharString::push_str()`].
    #[inline]
    pub fn try_push_str(&mut self, string: &str) -> Result<(), ReserveError> {
        self.0.push_str(string)
    }

    /// Removes a [`char`] from the [`CharString`] at a byte position and returns it.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions:
    ///
    /// 1. `idx` is larger than or equal tothe [`CharString`]'s length, or if it does not lie on a [`char`]
    /// 2. The system is out-of-memory when cloning the [`CharString`].
    ///
    /// For 2, if you want to handle such a problem manually, use [`CharString::try_remove()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("Hello 世界");
    ///
    /// assert_eq!(s.remove(6), '世');
    /// assert_eq!(s.remove(1), 'e');
    ///
    /// assert_eq!(s, "Hllo 界");
    /// ```
    /// ## Past total length:
    ///
    /// ```should_panic
    /// # use char_str::CharString;
    /// let mut c = CharString::from("hello there!");
    /// c.remove(12);
    /// ```
    ///
    /// ## Not on char boundary:
    ///
    /// ```should_panic
    /// # use char_str::CharString;
    /// let mut c = CharString::from("🦄");
    /// c.remove(1);
    /// ```
    #[inline]
    pub fn remove(&mut self, idx: usize) -> char {
        self.try_remove(idx).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::remove()`].
    ///
    /// This method won't panic if the system is out-of-memory, but return an [`ReserveError`].
    /// Otherwise it behaves the same as [`CharString::remove()`].
    ///
    /// # Panics
    ///
    /// This method still panics if the `idx` is larger than or equal to the [`CharString`]'s
    /// length, or if it does not lie on a [`char`] boundary.
    #[inline]
    pub fn try_remove(&mut self, idx: usize) -> Result<char, ReserveError> {
        self.0.remove(idx)
    }

    /// Retains only the characters specified by the `predicate`.
    ///
    /// If the `predicate` returns `true`, the character is kept, otherwise it is removed.
    ///
    /// # Panics
    ///
    /// Panics if the system is out-of-memory when cloning the [`CharString`]. If you want to
    /// handle such a problem manually, use [`CharString::try_retain()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("äb𝄞d€");
    ///
    /// let keep = [false, true, true, false, true];
    /// let mut iter = keep.iter();
    /// s.retain(|_| *iter.next().unwrap());
    ///
    /// assert_eq!(s, "b𝄞€");
    /// ```
    #[inline]
    pub fn retain(&mut self, predicate: impl FnMut(char) -> bool) {
        self.try_retain(predicate).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::retain()`].
    ///
    /// This method won't panic if the system is out-of-memory, but return an [`ReserveError`].
    #[inline]
    pub fn try_retain(&mut self, predicate: impl FnMut(char) -> bool) -> Result<(), ReserveError> {
        self.0.retain(predicate)
    }

    /// Inserts a character into the [`CharString`] at a byte position.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions:
    ///
    /// 1. `idx` is larger than the [`CharString`]'s length, or if it does not lie on a [`char`]
    ///    boundary.
    /// 2. The system is out-of-memory when cloning the [`CharString`].
    /// 3. The resulting length is greater than `2^56 - 1` on a 64-bit architecture or
    ///    `2^31 - 16` (`2_147_483_632`) on a 32-bit architecture.
    ///
    /// For 2 and 3, if you want to handle such a problem manually, use [`CharString::try_insert()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("Hello world");
    ///
    /// s.insert(11, '!');
    /// assert_eq!("Hello world!", s);
    ///
    /// s.insert(5, ',');
    /// assert_eq!("Hello, world!", s);
    /// ```
    #[inline]
    pub fn insert(&mut self, idx: usize, ch: char) {
        self.try_insert(idx, ch).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::insert()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` becomes too large
    /// by inserting a character, but return an [`ReserveError`]. Otherwise it behaves the same as
    /// [`CharString::insert()`].
    ///
    /// # Panics
    ///
    /// This method still panics if the `idx` is larger than the [`CharString`]'s length, or if it
    /// does not lie on a [`char`] boundary.
    #[inline]
    pub fn try_insert(&mut self, idx: usize, ch: char) -> Result<(), ReserveError> {
        self.0.insert_str(idx, ch.encode_utf8(&mut [0; 4]))
    }

    /// Inserts a string slice into the [`CharString`] at a byte position.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions:
    ///
    /// 1. `idx` is larger than the [`CharString`]'s length, or if it does not lie on a [`char`] boundary.
    /// 2. The system is out-of-memory when cloning the [`CharString`].
    /// 3. The resulting length is greater than `2^56 - 1` on a 64-bit architecture or
    ///    `2^31 - 16` (`2_147_483_632`) on a 32-bit architecture.
    ///
    /// For 2 and 3, if you want to handle such a problem manually, use [`CharString::try_insert_str()`].
    ///
    /// # Examples
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("bar");
    /// s.insert_str(0, "foo");
    /// assert_eq!("foobar", s);
    /// ```
    #[inline]
    pub fn insert_str(&mut self, idx: usize, string: &str) {
        self.try_insert_str(idx, string).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::insert_str()`].
    ///
    /// This method won't panic if the system is out-of-memory, or the `capacity` becomes too large
    /// by inserting a string slice, but return an [`ReserveError`]. Otherwise it behaves the same
    /// as [`CharString::insert_str()`].
    ///
    /// # Panics
    ///
    /// This method still panics if the `idx` is larger than the [`CharString`]'s length, or if it
    /// does not lie on a [`char`] boundary.
    #[inline]
    pub fn try_insert_str(&mut self, idx: usize, string: &str) -> Result<(), ReserveError> {
        self.0.insert_str(idx, string)
    }

    /// Creates a new [`CharString`] by repeating `self` `n` times.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions is met:
    ///
    /// 1. The resulting capacity would overflow (`self.len() * n` exceeds `usize::MAX`).
    /// 2. The system is out-of-memory.
    /// 3. The resulting length is greater than `2^56 - 1` on a 64-bit architecture or
    ///    `2^31 - 16` (`2_147_483_632`) on a 32-bit architecture.
    ///
    /// If you want to handle such a problem manually, use [`CharString::try_repeat()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::from("abc");
    /// assert_eq!(s.repeat(4), "abcabcabcabc");
    /// assert_eq!(s.repeat(0), "");
    /// ```
    #[inline]
    pub fn repeat(&self, n: usize) -> Self {
        self.try_repeat(n).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::repeat()`].
    ///
    /// This method won't panic, but returns a [`ReserveError`] if the capacity would overflow,
    /// the system is out-of-memory, or the resulting length exceeds the maximum. Otherwise it
    /// behaves the same as [`CharString::repeat()`].
    #[inline]
    pub fn try_repeat(&self, n: usize) -> Result<Self, ReserveError> {
        if n == 0 || self.is_empty() {
            Ok(CharString::new())
        } else if n == 1 {
            Ok(self.clone())
        } else {
            let capacity = self.len().checked_mul(n).ok_or(ReserveError)?;
            let mut res = CharString::try_with_capacity(capacity)?;
            for _ in 0..n {
                res.try_push_str(self)?;
            }
            Ok(res)
        }
    }

    /// Shortens a [`CharString`] to the specified length.
    ///
    /// If `new_len` is greater than or equal to the string's current length, this has no effect.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions is met:
    ///
    /// 1. `new_len` does not lie on a [`char`] boundary.
    /// 2. The system is out-of-memory when cloning the [`CharString`].
    ///
    /// For 2, If you want to handle such a problem manually, use [`CharString::try_truncate()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("hello");
    /// s.truncate(2);
    /// assert_eq!(s, "he");
    ///
    /// // Truncating to a larger length does nothing:
    /// s.truncate(10);
    /// assert_eq!(s, "he");
    /// ```
    #[inline]
    pub fn truncate(&mut self, new_len: usize) {
        self.try_truncate(new_len).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::truncate()`].
    ///
    /// This method won't panic if the system is out-of-memory, but return an [`ReserveError`].
    /// Otherwise it behaves the same as [`CharString::truncate()`].
    ///
    /// # Panics
    ///
    /// This method still panics if `new_len` does not lie on a [`char`] boundary.
    #[inline]
    pub fn try_truncate(&mut self, new_len: usize) -> Result<(), ReserveError> {
        self.0.truncate(new_len)
    }

    /// Splits the string into two at the given byte index.
    ///
    /// Returns a newly allocated [`CharString`]. `self` contains bytes `[0, at)`, and
    /// the returned [`CharString`] contains bytes `[at, len)`. `at` must be on the
    /// boundary of a UTF-8 code point.
    ///
    /// # Panics
    ///
    /// Panics if **any** of the following conditions is met:
    ///
    /// 1. `at` does not lie on a [`char`] boundary, or is beyond the end of the string.
    /// 2. The system is out-of-memory when creating the new [`CharString`].
    ///
    /// For 2, if you want to handle such a problem manually, use [`CharString::try_split_off()`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut hello = CharString::from("Hello, World!");
    /// let world = hello.split_off(7);
    /// assert_eq!(hello, "Hello, ");
    /// assert_eq!(world, "World!");
    /// ```
    #[inline]
    #[must_use = "use `.truncate()` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self {
        self.try_split_off(at).unwrap_with_msg()
    }

    /// Fallible version of [`CharString::split_off()`].
    ///
    /// This method won't panic if the system is out-of-memory, but returns a [`ReserveError`].
    /// Otherwise it behaves the same as [`CharString::split_off()`].
    ///
    /// # Panics
    ///
    /// This method still panics if `at` does not lie on a [`char`] boundary, or is beyond the
    /// end of the string.
    #[inline]
    #[must_use = "use `.try_truncate()` if you don't need the other half"]
    pub fn try_split_off(&mut self, at: usize) -> Result<Self, ReserveError> {
        let other = CharString(Repr::from_str(&self.as_str()[at..])?);
        self.try_truncate(at)?;
        Ok(other)
    }

    /// Reduces the length of the [`CharString`] to zero without allocating.
    ///
    /// If the [`CharString`] has a unique growable buffer, this method will not change the
    /// capacity. Shared heap buffers are released, leaving an empty inline [`CharString`].
    ///
    /// # Examples
    ///
    /// ## unique growable buffer
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("This is a example of unique CharString");
    /// assert_eq!(s.capacity(), 38);
    ///
    /// s.clear();
    ///
    /// assert_eq!(s, "");
    /// assert_eq!(s.capacity(), 38);
    /// ```
    ///
    /// ## shared growable buffer
    ///
    /// ```
    /// # use char_str::CharString;
    /// let mut s = CharString::from("This is a example of not unique CharString");
    /// assert_eq!(s.capacity(), 42);
    ///
    /// let s2 = s.clone();
    /// s.clear();
    ///
    /// assert_eq!(s, "");
    /// assert_eq!(s.capacity(), 2 * size_of::<usize>());
    /// ```
    #[inline]
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Returns whether the [`CharString`] is heap-allocated.
    ///
    /// # Examples
    ///
    /// ## inline
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::from("hello");
    /// assert!(!s.is_heap_allocated());
    /// ```
    ///
    /// ## heap
    ///
    /// ```
    /// # use char_str::CharString;
    /// let s = CharString::from("More than 2 * size_of::<usize>() bytes is heap-allocated");
    /// assert!(s.is_heap_allocated());
    /// ```
    #[inline]
    pub fn is_heap_allocated(&self) -> bool {
        self.0.is_heap_buffer()
    }

    #[cfg(feature = "get-size")]
    pub(crate) fn heap_allocation_size(&self) -> usize {
        self.0.heap_allocation_size()
    }
}

/// A [`Clone`] implementation for [`CharString`].
///
/// The clone operation is performed using a reference counting mechanism, which means:
/// - The cloned string shares the same underlying data with the original string
/// - The cloning process is very efficient (O(1) time complexity)
/// - No memory allocation occurs during cloning
///
/// # Examples
///
/// ```
/// # use char_str::CharString;
/// let s1 = CharString::from("Hello, World!");
/// let s2 = s1.clone();
///
/// assert_eq!(s1, s2);
/// ```
impl Clone for CharString {
    #[inline]
    fn clone(&self) -> Self {
        CharString(self.0.make_shallow_clone())
    }

    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.0.replace_inner(source.0.make_shallow_clone());
    }
}

/// A [`Drop`] implementation for [`CharString`].
///
/// If the string is heap-allocated, dropping it decrements the reference count and frees the heap
/// memory when the last reference is dropped.
impl Drop for CharString {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: The representation is never accessed again after `drop` returns.
        unsafe { self.0.release_for_drop() };
    }
}

// SAFETY: `CharString` is `repr(transparent)` over `Repr`, and `Repr` works like `Arc`.
unsafe impl Send for CharString {}
unsafe impl Sync for CharString {}

impl Default for CharString {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for CharString {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for CharString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for CharString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl AsRef<str> for CharString {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[cfg(feature = "std")]
impl AsRef<OsStr> for CharString {
    #[inline]
    fn as_ref(&self) -> &OsStr {
        OsStr::new(self.as_str())
    }
}

impl AsRef<[u8]> for CharString {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Borrow<str> for CharString {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Eq for CharString {}

impl PartialEq for CharString {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0.content_eq(&other.0)
    }
}

impl PartialEq<str> for CharString {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str().eq(other)
    }
}

impl PartialEq<CharString> for str {
    #[inline]
    fn eq(&self, other: &CharString) -> bool {
        self.eq(other.as_str())
    }
}

impl PartialEq<&str> for CharString {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str().eq(*other)
    }
}

impl PartialEq<CharString> for &str {
    #[inline]
    fn eq(&self, other: &CharString) -> bool {
        (*self).eq(other.as_str())
    }
}

impl PartialEq<String> for CharString {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl PartialEq<CharString> for String {
    #[inline]
    fn eq(&self, other: &CharString) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl PartialEq<Cow<'_, str>> for CharString {
    #[inline]
    fn eq(&self, other: &Cow<'_, str>) -> bool {
        self.as_str().eq(other.as_ref())
    }
}

impl PartialEq<CharString> for Cow<'_, str> {
    #[inline]
    fn eq(&self, other: &CharString) -> bool {
        self.as_ref().eq(other.as_str())
    }
}

impl Ord for CharString {
    #[inline]
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.0.content_cmp(&other.0)
    }
}

impl PartialOrd for CharString {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for CharString {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

impl From<char> for CharString {
    #[inline]
    #[track_caller]
    fn from(value: char) -> Self {
        CharString(Repr::from_char(value))
    }
}

impl From<&str> for CharString {
    #[inline]
    #[track_caller]
    fn from(value: &str) -> Self {
        CharString(Repr::from_str(value).unwrap_with_msg())
    }
}

impl From<String> for CharString {
    #[inline]
    #[track_caller]
    fn from(value: String) -> Self {
        CharString(Repr::from_str(&value).unwrap_with_msg())
    }
}

impl From<&String> for CharString {
    #[inline]
    #[track_caller]
    fn from(value: &String) -> Self {
        CharString(Repr::from_str(value).unwrap_with_msg())
    }
}

impl From<Cow<'_, str>> for CharString {
    fn from(cow: Cow<str>) -> Self {
        match cow {
            Cow::Borrowed(s) => s.into(),
            Cow::Owned(s) => s.into(),
        }
    }
}

impl From<Box<str>> for CharString {
    #[inline]
    #[track_caller]
    fn from(value: Box<str>) -> Self {
        CharString(Repr::from_str(&value).unwrap_with_msg())
    }
}

impl From<&CharString> for CharString {
    #[inline]
    fn from(value: &CharString) -> Self {
        value.clone()
    }
}

impl From<CharString> for String {
    #[inline]
    fn from(value: CharString) -> Self {
        value.as_str().into()
    }
}

impl From<&CharString> for String {
    #[inline]
    fn from(value: &CharString) -> Self {
        value.as_str().into()
    }
}

impl FromStr for CharString {
    type Err = ReserveError;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Repr::from_str(s).map(Self)
    }
}

impl FromIterator<char> for CharString {
    fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
        let iter = iter.into_iter();

        let (lower_bound, _) = iter.size_hint();
        // If reserving `lower_bound` fails, fall back to empty and hope it was inaccurate
        let mut buf = CharString::try_with_capacity(lower_bound).unwrap_or_default();

        for ch in iter {
            buf.push_str(ch.encode_utf8(&mut [0; 4]));
        }
        buf
    }
}

impl<'a> FromIterator<&'a char> for CharString {
    fn from_iter<T: IntoIterator<Item = &'a char>>(iter: T) -> Self {
        iter.into_iter().copied().collect()
    }
}

impl<'a> FromIterator<&'a str> for CharString {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        let mut buf = CharString::new();
        buf.extend(iter);
        buf
    }
}

impl FromIterator<Box<str>> for CharString {
    fn from_iter<I: IntoIterator<Item = Box<str>>>(iter: I) -> Self {
        let mut buf = CharString::new();
        buf.extend(iter);
        buf
    }
}

impl<'a> FromIterator<Cow<'a, str>> for CharString {
    fn from_iter<I: IntoIterator<Item = Cow<'a, str>>>(iter: I) -> Self {
        let mut buf = CharString::new();
        buf.extend(iter);
        buf
    }
}

impl FromIterator<String> for CharString {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        let mut buf = CharString::new();
        buf.extend(iter);
        buf
    }
}

impl FromIterator<CharString> for CharString {
    fn from_iter<T: IntoIterator<Item = CharString>>(iter: T) -> Self {
        let mut iter = iter.into_iter();
        let Some(mut buf) = iter.next() else {
            return CharString::new();
        };
        buf.extend(iter);
        buf
    }
}

impl Extend<char> for CharString {
    fn extend<T: IntoIterator<Item = char>>(&mut self, iter: T) {
        let iter = iter.into_iter();

        let (lower_bound, _) = iter.size_hint();
        // Ignore the error and hope that the lower_bound is incorrect.
        let _ = self.try_reserve(lower_bound);

        for ch in iter {
            self.push(ch);
        }
    }
}

impl<'a> Extend<&'a char> for CharString {
    fn extend<T: IntoIterator<Item = &'a char>>(&mut self, iter: T) {
        self.extend(iter.into_iter().copied());
    }
}

impl<'a> Extend<&'a str> for CharString {
    fn extend<T: IntoIterator<Item = &'a str>>(&mut self, iter: T) {
        iter.into_iter().for_each(|s| self.push_str(s));
    }
}

impl Extend<Box<str>> for CharString {
    fn extend<T: IntoIterator<Item = Box<str>>>(&mut self, iter: T) {
        iter.into_iter().for_each(move |s| self.push_str(&s));
    }
}

impl<'a> Extend<Cow<'a, str>> for CharString {
    fn extend<T: IntoIterator<Item = Cow<'a, str>>>(&mut self, iter: T) {
        iter.into_iter().for_each(move |s| self.push_str(&s));
    }
}

impl Extend<String> for CharString {
    fn extend<T: IntoIterator<Item = String>>(&mut self, iter: T) {
        iter.into_iter().for_each(move |s| self.push_str(&s));
    }
}

impl Extend<CharString> for CharString {
    fn extend<T: IntoIterator<Item = CharString>>(&mut self, iter: T) {
        for s in iter {
            self.push_str(&s);
        }
    }
}

impl Extend<CharString> for String {
    fn extend<T: IntoIterator<Item = CharString>>(&mut self, iter: T) {
        for s in iter {
            self.push_str(&s);
        }
    }
}

impl fmt::Write for CharString {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_str(s);
        Ok(())
    }

    #[inline]
    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        match args.as_str() {
            Some(s) => {
                if self.is_empty() && !self.is_heap_allocated() {
                    // Since self is empty inline buffer or empty static buffer, constructing a new
                    // one with `from_static_str` is more efficient since it is O(1).
                    *self = CharString::from_static_str(s);
                } else {
                    self.push_str(s);
                }
                Ok(())
            }
            None => fmt::write(self, args),
        }
    }
}

impl Add<&str> for CharString {
    type Output = Self;

    #[inline]
    fn add(mut self, rhs: &str) -> Self::Output {
        self.push_str(rhs);
        self
    }
}

impl AddAssign<&str> for CharString {
    #[inline]
    fn add_assign(&mut self, rhs: &str) {
        self.push_str(rhs);
    }
}

trait UnwrapWithMsg {
    type T;
    fn unwrap_with_msg(self) -> Self::T;
}

impl<T, E: fmt::Display> UnwrapWithMsg for Result<T, E> {
    type T = T;
    #[inline(always)]
    #[track_caller]
    fn unwrap_with_msg(self) -> T {
        #[inline(never)]
        #[cold]
        #[track_caller]
        fn do_panic_with_msg<E: fmt::Display>(error: E) -> ! {
            panic!("{error}")
        }

        match self {
            Ok(value) => value,
            Err(err) => do_panic_with_msg(err),
        }
    }
}
