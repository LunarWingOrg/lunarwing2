use core::{borrow::Borrow, cmp, fmt, hash::Hash, hash::Hasher, mem, ops::Deref, str::FromStr};

use alloc::{borrow::Cow, boxed::Box, string::String};

#[cfg(feature = "std")]
use std::ffi::OsStr;

use crate::{CharString, Repr, ReserveError, UnwrapWithMsg};

/// Compact, immutable, UTF-8 encoded owned string type.
///
/// Values constructed directly store short strings inline. Long strings use an exactly-sized,
/// reference-counted heap allocation without a capacity field, making clones cheap without
/// retaining capacity that can never be used. Freezing a heap-allocated [`CharString`] preserves
/// the heap storage kind even when its current contents fit inline. Use
/// [`CharStr::into_char_string`] to convert heap storage to the growable layout used by
/// [`CharString`].
#[repr(transparent)]
pub struct CharStr(Repr);

const _: () = {
    assert!(size_of::<CharStr>() == size_of::<[usize; 2]>());
    assert!(size_of::<Option<CharStr>>() == size_of::<[usize; 2]>());
    assert!(align_of::<CharStr>() == align_of::<usize>());
    assert!(align_of::<Option<CharStr>>() == align_of::<usize>());
};

impl CharStr {
    /// Maximum number of UTF-8 bytes that can be stored inline.
    pub const INLINE_CAPACITY: usize = crate::repr::MAX_INLINE_SIZE;

    /// Creates an empty `CharStr`.
    #[inline]
    pub const fn new() -> Self {
        Self(Repr::new())
    }

    /// Creates an inline `CharStr`, returning `None` if `text` does not fit inline.
    #[inline]
    pub const fn new_inline(text: &str) -> Option<Self> {
        match Repr::from_inline_str(text) {
            Some(repr) => Some(Self(repr)),
            None => None,
        }
    }

    /// Creates an exactly-sized, heap-allocated `CharStr`.
    ///
    /// This always allocates, even when `text` fits inline. To handle allocation failure, use
    /// [`CharStr::try_new_heap`].
    ///
    /// # Panics
    ///
    /// Panics if `text` is too long or the exact buffer cannot be allocated.
    #[inline]
    pub fn new_heap(text: &str) -> Self {
        Self::try_new_heap(text).unwrap_with_msg()
    }

    /// Fallible version of [`CharStr::new_heap`].
    #[inline]
    pub fn try_new_heap(text: &str) -> Result<Self, ReserveError> {
        Repr::from_exact_heap_str(text).map(Self)
    }

    /// Creates a `CharStr` backed directly by a static string when it does not fit inline.
    ///
    /// # Panics
    ///
    /// Panics if `text` is longer than `2^56 - 1` bytes on a 64-bit architecture or
    /// `2^24 - 1` (`16_777_215`) bytes on a 32-bit architecture.
    #[inline]
    pub const fn from_static_str(text: &'static str) -> Self {
        match Repr::from_static_str(text) {
            Ok(repr) => Self(repr),
            Err(_) => panic!("text is too long"),
        }
    }

    /// Creates an exactly-sized `CharStr`.
    #[inline]
    pub fn try_from_str(text: &str) -> Result<Self, ReserveError> {
        Repr::from_exact_str(text).map(Self)
    }

    /// Creates an exactly-sized `CharStr` by concatenating string slices.
    ///
    /// Heap storage is allocated at most once. To handle allocation failure, use
    /// [`CharStr::try_concat`].
    ///
    /// # Panics
    ///
    /// Panics if the combined length overflows or the exact buffer cannot be allocated.
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharStr;
    /// let path = CharStr::concat(&["prefix", "suffix"]);
    /// assert_eq!(path, "prefixsuffix");
    /// ```
    #[inline]
    pub fn concat<T: AsRef<str>>(slices: &[T]) -> Self {
        Self::try_concat(slices).unwrap_with_msg()
    }

    /// Fallible version of [`CharStr::concat`].
    ///
    /// Returns a [`ReserveError`] if the combined length overflows, the exact buffer cannot be
    /// allocated, or an [`AsRef`] implementation reports inconsistent slice lengths while the
    /// result is constructed.
    #[inline]
    pub fn try_concat<T: AsRef<str>>(slices: &[T]) -> Result<Self, ReserveError> {
        Self::try_join(slices, "")
    }

    /// Creates an exactly-sized `CharStr` by joining string slices with a separator.
    ///
    /// Heap storage is allocated at most once. To handle allocation failure, use
    /// [`CharStr::try_join`].
    ///
    /// # Panics
    ///
    /// Panics if the joined length overflows or the exact buffer cannot be allocated.
    ///
    /// # Examples
    ///
    /// ```
    /// # use char_str::CharStr;
    /// let name = CharStr::join(&["package", "module", "name"], ".");
    /// assert_eq!(name, "package.module.name");
    /// ```
    #[inline]
    pub fn join<T: AsRef<str>>(slices: &[T], separator: &str) -> Self {
        Self::try_join(slices, separator).unwrap_with_msg()
    }

    /// Fallible version of [`CharStr::join`].
    ///
    /// Returns a [`ReserveError`] if the joined length overflows, the exact buffer cannot be
    /// allocated, or an [`AsRef`] implementation reports inconsistent slice lengths while the
    /// result is constructed.
    #[inline]
    pub fn try_join<T: AsRef<str>>(slices: &[T], separator: &str) -> Result<Self, ReserveError> {
        Repr::from_exact_joined_slices(slices, separator).map(Self)
    }

    /// Returns the string as a string slice.
    #[inline]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns the string as a byte slice.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Returns the length in bytes.
    #[inline]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the string has a length of zero.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns whether the string uses a reference-counted heap allocation.
    #[inline]
    pub const fn is_heap_allocated(&self) -> bool {
        self.0.is_heap_buffer()
    }

    #[cfg(feature = "get-size")]
    pub(crate) fn heap_allocation_size(&self) -> usize {
        self.0.heap_allocation_size()
    }

    /// Converts this value into a mutable [`CharString`].
    ///
    /// Unique exact heap storage is converted to growable storage with `realloc`. Shared heap
    /// storage is copied into a new growable allocation.
    ///
    /// # Panics
    ///
    /// Panics if reallocating or copying heap storage fails. To handle allocation failure, use
    /// [`CharStr::try_into_char_string`].
    #[inline]
    pub fn into_char_string(self) -> CharString {
        self.try_into_char_string().unwrap_with_msg()
    }

    /// Tries to convert this value into a mutable [`CharString`].
    ///
    /// Inline and static storage are transferred without reallocating. Unique exact heap storage
    /// is converted with `realloc`, while shared heap storage is copied.
    ///
    /// # Errors
    ///
    /// Returns a [`ReserveError`] if converting heap storage fails. Because this method consumes
    /// `self`, the original [`CharStr`] is dropped on failure.
    #[inline]
    pub fn try_into_char_string(mut self) -> Result<CharString, ReserveError> {
        self.0.make_growable()?;
        Ok(CharString::from_repr(mem::replace(&mut self.0, Repr::new())))
    }

    pub(crate) fn from_repr(repr: Repr) -> Self {
        debug_assert!(!repr.is_growable_heap_buffer());
        Self(repr)
    }
}

impl Clone for CharStr {
    #[inline]
    fn clone(&self) -> Self {
        Self(self.0.make_shallow_clone())
    }

    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.0.replace_inner(source.0.make_shallow_clone());
    }
}

impl Drop for CharStr {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: The representation is never accessed again after `drop` returns.
        unsafe { self.0.release_for_drop() };
    }
}

// SAFETY: `CharStr` is `repr(transparent)` over `Repr`, and heap storage is immutable and
// reference-counted like `Arc`.
unsafe impl Send for CharStr {}
unsafe impl Sync for CharStr {}

impl Default for CharStr {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for CharStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Debug for CharStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for CharStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl AsRef<str> for CharStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[cfg(feature = "std")]
impl AsRef<OsStr> for CharStr {
    #[inline]
    fn as_ref(&self) -> &OsStr {
        OsStr::new(self.as_str())
    }
}

impl AsRef<[u8]> for CharStr {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Borrow<str> for CharStr {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Eq for CharStr {}

impl PartialEq for CharStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0.content_eq(&other.0)
    }
}

impl PartialEq<str> for CharStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<CharStr> for str {
    #[inline]
    fn eq(&self, other: &CharStr) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for CharStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<CharStr> for &str {
    #[inline]
    fn eq(&self, other: &CharStr) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for CharStr {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<CharStr> for String {
    #[inline]
    fn eq(&self, other: &CharStr) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<CharString> for CharStr {
    #[inline]
    fn eq(&self, other: &CharString) -> bool {
        self.0.content_eq(&other.0)
    }
}

impl PartialEq<CharStr> for CharString {
    #[inline]
    fn eq(&self, other: &CharStr) -> bool {
        self.0.content_eq(&other.0)
    }
}

impl Ord for CharStr {
    #[inline]
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.0.content_cmp(&other.0)
    }
}

impl PartialOrd for CharStr {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for CharStr {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl From<char> for CharStr {
    #[inline]
    fn from(value: char) -> Self {
        Self(Repr::from_char(value))
    }
}

impl From<&str> for CharStr {
    #[inline]
    #[track_caller]
    fn from(value: &str) -> Self {
        Self::try_from_str(value).unwrap_with_msg()
    }
}

impl From<String> for CharStr {
    #[inline]
    #[track_caller]
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<&String> for CharStr {
    #[inline]
    #[track_caller]
    fn from(value: &String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<Cow<'_, str>> for CharStr {
    fn from(value: Cow<'_, str>) -> Self {
        Self::from(value.as_ref())
    }
}

impl From<Box<str>> for CharStr {
    #[inline]
    #[track_caller]
    fn from(value: Box<str>) -> Self {
        Self::from(value.as_ref())
    }
}

impl From<&CharStr> for CharStr {
    #[inline]
    fn from(value: &CharStr) -> Self {
        value.clone()
    }
}

impl From<CharStr> for String {
    #[inline]
    fn from(value: CharStr) -> Self {
        value.as_str().into()
    }
}

impl From<&CharStr> for String {
    #[inline]
    fn from(value: &CharStr) -> Self {
        value.as_str().into()
    }
}

impl From<CharString> for CharStr {
    #[inline]
    fn from(value: CharString) -> Self {
        value.freeze()
    }
}

impl From<CharStr> for CharString {
    #[inline]
    fn from(value: CharStr) -> Self {
        value.into_char_string()
    }
}

impl FromStr for CharStr {
    type Err = ReserveError;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from_str(s)
    }
}

impl FromIterator<char> for CharStr {
    fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
        CharString::from_iter(iter).freeze()
    }
}

impl<'a> FromIterator<&'a char> for CharStr {
    fn from_iter<T: IntoIterator<Item = &'a char>>(iter: T) -> Self {
        iter.into_iter().copied().collect()
    }
}
