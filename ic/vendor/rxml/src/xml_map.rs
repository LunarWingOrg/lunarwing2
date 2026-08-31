/*!
# Map structure for XML names to values

This module contains the supporting structures for the
[`XmlMap`][`crate::XmlMap`] struct, which is used to represent the set of
attributes on an XML element.

It is used for instance in [`Event`][`crate::Event`].
*/
use alloc::collections::{btree_map, BTreeMap};
use alloc::string::String;
use core::borrow::Borrow;
use core::fmt;
use core::hash::Hash;
use core::iter::FromIterator;

use crate::{Namespace, NcName};

/// Container struct for a set of attributes on an XML element.
///
/// This is basically a wrapper around one of the map types from the standard
/// library (which one is an implementation detail).
///
/// It is provided instead of using `HashMap<(Namespace, NcName), V>` in
/// the public API for the following reasons:
///
/// - Flexibility regarding the map implementation
/// - Ease of use: standard library maps are excruciatingly annoying to use
///   with tuple keys.
///
/// Other than that, it behaves and offers the same API like a standard
/// library map.
#[derive(Clone, Default)]
pub struct XmlMap<V> {
	inner: BTreeMap<Namespace<'static>, BTreeMap<NcName, V>>,
}

/// Type alias for the commonly used mapping type for XML attribute values.
pub type AttrMap = XmlMap<String>;

impl<V> XmlMap<V> {
	/// Creates an empty `XmlMap`.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let map = AttrMap::new();
	/// assert_eq!(map.len(), 0);
	/// ```
	#[inline(always)]
	pub fn new() -> Self {
		Self {
			inner: BTreeMap::new(),
		}
	}

	/// Clears the map, removing all key-value pairs.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// assert_eq!(map.len(), 1);
	/// map.clear();
	/// assert_eq!(map.len(), 0);
	/// ```
	#[inline(always)]
	pub fn clear(&mut self) {
		self.inner.clear()
	}

	/// Returns a reference to the value corresponding to the attribute.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// assert_eq!(map.get(&Namespace::NONE, "bar").unwrap(), "hello");
	/// assert!(map.get(&Namespace::NONE, "baz").is_none());
	/// assert!(map.get("other", "baz").is_none());
	/// ```
	#[inline(always)]
	pub fn get<'a, NS: Ord + Hash + Eq + ?Sized, N: Ord + Hash + Eq + ?Sized>(
		&'a self,
		namespace: &NS,
		name: &N,
	) -> Option<&'a V>
	where
		Namespace<'static>: Borrow<NS>,
		NcName: Borrow<N>,
	{
		self.inner.get(namespace).and_then(|inner| inner.get(name))
	}

	/// Returns `true` if the map contains a value for the specified
	/// attribute.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// assert!(map.contains_key(&Namespace::NONE, "bar"));
	/// assert!(!map.contains_key(&Namespace::NONE, "baz"));
	/// assert!(!map.contains_key("other", "baz"));
	/// ```
	#[inline(always)]
	pub fn contains_key<NS: Ord + Hash + Eq + ?Sized, N: Ord + Hash + Eq + ?Sized>(
		&self,
		namespace: &NS,
		name: &N,
	) -> bool
	where
		Namespace<'static>: Borrow<NS>,
		NcName: Borrow<N>,
	{
		self.inner
			.get(namespace)
			.map(|inner| inner.contains_key(name))
			.unwrap_or(false)
	}

	/// Returns a mutable reference to the value corresponding to the
	/// attribute.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// map.get_mut(&Namespace::NONE, "bar").map(|x| x.push_str(" world".try_into().unwrap()));
	/// assert_eq!(map.get(&Namespace::NONE, "bar").unwrap(), "hello world");
	/// ```
	pub fn get_mut<'a, NS: Ord + Hash + Eq + ?Sized, N: Ord + Hash + Eq + ?Sized>(
		&'a mut self,
		namespace: &NS,
		name: &N,
	) -> Option<&'a mut V>
	where
		Namespace<'static>: Borrow<NS>,
		NcName: Borrow<N>,
	{
		self.inner
			.get_mut(namespace)
			.and_then(|inner| inner.get_mut(name))
	}

	/// Insert an attribute
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// assert_eq!(map.get(&Namespace::NONE, "bar").unwrap(), "hello");
	/// ```
	pub fn insert(&mut self, namespace: Namespace<'static>, name: NcName, value: V) -> Option<V> {
		match self.inner.entry(namespace) {
			btree_map::Entry::Occupied(mut o) => o.get_mut().insert(name, value),
			btree_map::Entry::Vacant(v) => v.insert(BTreeMap::new()).insert(name, value),
		}
	}

	/// Removes an attribute from the map, returning the value of the
	/// attribute if it was previously in the map.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// assert!(map.remove(&Namespace::NONE, "baz").is_none());
	/// assert_eq!(map.remove(&Namespace::NONE, "bar").unwrap(), "hello");
	/// assert!(map.remove(&Namespace::NONE, "bar").is_none());
	/// ```
	#[inline(always)]
	pub fn remove<NS: Ord + Hash + Eq + ?Sized, N: Ord + Hash + Eq + ?Sized>(
		&mut self,
		namespace: &NS,
		name: &N,
	) -> Option<V>
	where
		Namespace<'static>: Borrow<NS>,
		NcName: Borrow<N>,
	{
		match self.inner.get_mut(namespace) {
			None => None,
			Some(inner) => {
				let result = inner.remove(name);
				if inner.is_empty() {
					self.inner.remove(namespace);
				}
				result
			}
		}
	}

	/// Retains only the elements specified by the predicate.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value 1".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value 2".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value 3".try_into().unwrap());
	/// map.retain(|_ns, name, _value| name == "bar");
	/// assert_eq!(map.len(), 2);
	/// ```
	#[inline(always)]
	pub fn retain<F: FnMut(&Namespace, &NcName, &mut V) -> bool>(&mut self, mut f: F) {
		self.inner.retain(|ns, inner| {
			inner.retain(|name, value| f(ns, name, value));
			!inner.is_empty()
		})
	}

	/// An iterator visiting all attribute namespace/name pairs in arbitrary
	/// order.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value 1".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value 2".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value 3".try_into().unwrap());
	/// let data: Vec<_> = map.into_names().collect();
	/// assert_eq!(data.len(), 3);
	/// assert_eq!(data.iter().filter(|(ns, _)| ns.is_none()).count(), 2);
	/// assert_eq!(data.iter().filter(|(ns, _)| !ns.is_none()).count(), 1);
	/// ```
	#[inline(always)]
	pub fn into_names(self) -> IntoNames<V> {
		IntoNames::new(self.inner.into_iter())
	}

	/// An iterator visiting all attribute values in arbitrary order.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// let data: Vec<_> = map.into_values().collect();
	/// let str_data: Vec<&str> = data.iter().map(|x| x.as_str()).collect();
	/// assert_eq!(&str_data, &["value", "value", "value"]);
	/// ```
	#[inline(always)]
	pub fn into_values(self) -> IntoValues<V> {
		IntoValues::new(self.inner.into_values())
	}

	/// An iterator visiting all attribute pairs in arbitrary order.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// let data: Vec<_> = map.iter().collect();
	/// assert_eq!(data.len(), 3);
	/// assert_eq!(data.iter().filter(|((ns, _), _)| ns.is_none()).count(), 2);
	/// assert_eq!(data.iter().filter(|((ns, _), _)| !ns.is_none()).count(), 1);
	/// ```
	#[inline(always)]
	pub fn iter(&self) -> Iter<'_, V> {
		self.into_iter()
	}

	/// An iterator visiting all attribute pairs in arbitrary order, with
	/// mutable references to the values.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// let data: Vec<_> = map.iter_mut().collect();
	/// assert_eq!(data.len(), 3);
	/// assert_eq!(data.iter().filter(|((ns, _), _)| ns.is_none()).count(), 2);
	/// assert_eq!(data.iter().filter(|((ns, _), _)| !ns.is_none()).count(), 1);
	/// ```
	#[inline(always)]
	pub fn iter_mut(&mut self) -> IterMut<'_, V> {
		self.into_iter()
	}

	/// An iterator visiting all attribute namespace/name pairs in arbitrary
	/// order.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value 1".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value 2".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value 3".try_into().unwrap());
	/// let data: Vec<_> = map.names().collect();
	/// assert_eq!(data.len(), 3);
	/// assert_eq!(data.iter().filter(|(ns, _)| ns.is_none()).count(), 2);
	/// assert_eq!(data.iter().filter(|(ns, _)| !ns.is_none()).count(), 1);
	/// ```
	#[inline(always)]
	pub fn names(&self) -> Names<'_, V> {
		Names::new(self.inner.iter())
	}

	/// An iterator visiting all attribute values in arbitrary order.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// let data: Vec<_> = map.values().collect();
	/// let str_data: Vec<&str> = data.iter().map(|x| x.as_str()).collect();
	/// assert_eq!(&str_data, &["value", "value", "value"]);
	/// ```
	#[inline(always)]
	pub fn values(&self) -> Values<'_, V> {
		Values::new(self.inner.values())
	}

	/// An iterator visiting all attribute values as mutable references in
	/// arbitrary order.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert(Namespace::NONE, "baz".try_into().unwrap(), "value".try_into().unwrap());
	/// map.insert("other".try_into().unwrap(), "bar".try_into().unwrap(), "value".try_into().unwrap());
	/// let data: Vec<_> = map.values_mut().collect();
	/// let str_data: Vec<&str> = data.iter().map(|x| x.as_str()).collect();
	/// assert_eq!(&str_data, &["value", "value", "value"]);
	/// ```
	#[inline(always)]
	pub fn values_mut(&mut self) -> ValuesMut<'_, V> {
		ValuesMut::new(self.inner.values_mut())
	}

	/// Returns the number of attributes in the map.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// assert!(map.is_empty());
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// assert_eq!(map.len(), 1);
	/// ```
	#[inline(always)]
	pub fn len(&self) -> usize {
		self.inner.values().map(|x| x.len()).sum::<usize>()
	}

	/// Returns true if the map is empty.
	///
	/// # Example
	///
	/// ```
	/// # use rxml::{AttrMap, Namespace, NcName};
	/// let mut map = AttrMap::new();
	/// assert!(map.is_empty());
	/// map.insert(Namespace::NONE, "bar".try_into().unwrap(), "hello".try_into().unwrap());
	/// assert!(!map.is_empty());
	/// ```
	pub fn is_empty(&self) -> bool {
		self.inner.is_empty() || self.inner.values().all(|x| x.is_empty())
	}

	/// Gets the given key’s corresponding entry in the map for in-place
	/// manipulation.
	pub fn entry(&mut self, namespace: Namespace<'static>, name: NcName) -> Entry<'_, V> {
		match self.inner.entry(namespace) {
			btree_map::Entry::Vacant(entry) => {
				Entry::Vacant(VacantEntry(VacantInner::OuterVacant { entry, name }))
			}
			btree_map::Entry::Occupied(entry) => {
				let namespace = entry.key().clone();
				match entry.into_mut().entry(name) {
					btree_map::Entry::Vacant(entry) => {
						Entry::Vacant(VacantEntry(VacantInner::InnerVacant { namespace, entry }))
					}
					btree_map::Entry::Occupied(entry) => {
						Entry::Occupied(OccupiedEntry { namespace, entry })
					}
				}
			}
		}
	}
}

impl<V: fmt::Debug> fmt::Debug for XmlMap<V> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_map().entries(self.iter()).finish()
	}
}

impl<V: PartialEq> PartialEq for XmlMap<V> {
	fn eq(&self, other: &XmlMap<V>) -> bool {
		if self.len() != other.len() {
			return false;
		}
		for (lhs, rhs) in self.iter().zip(other.iter()) {
			if lhs != rhs {
				return false;
			}
		}
		true
	}
}

impl<V: Eq> Eq for XmlMap<V> {}

/// A view into an occupied entry in a `XmlMap`.
///
/// It is part of the [`Entry`] enum.
pub struct OccupiedEntry<'a, V> {
	namespace: Namespace<'static>,
	entry: btree_map::OccupiedEntry<'a, NcName, V>,
}

impl<'a, V> OccupiedEntry<'a, V> {
	/// Gets a reference to the value in the entry.
	#[inline(always)]
	pub fn get(&self) -> &V {
		self.entry.get()
	}

	/// Gets a mutable reference to the value in the entry.
	#[inline(always)]
	pub fn get_mut(&mut self) -> &mut V {
		self.entry.get_mut()
	}

	/// Sets the value of the entry with the `OccupiedEntry`'s key, and
	/// returns the entry's old value.
	#[inline(always)]
	pub fn insert(&mut self, value: V) -> V {
		self.entry.insert(value)
	}

	/// Converts the entry into a mutable reference to its value.
	///
	/// If you need multiple references to the `OccupiedEntry`, see
	/// [`get_mut`][`Self::get_mut`].
	#[inline(always)]
	pub fn into_mut(self) -> &'a mut V {
		self.entry.into_mut()
	}

	/// Gets a reference to the value in the entry.
	#[inline(always)]
	pub fn key(&self) -> (&Namespace<'static>, &NcName) {
		(&self.namespace, self.entry.key())
	}

	/// Takes the value of the entry out of the map, and returns it.
	#[inline(always)]
	pub fn remove(self) -> V {
		// XXX: this does not remove the parent map if it now is empty, unlike
		// other removal methods.
		self.entry.remove()
	}

	/// Takes ownership of the attribute namespace, name and value
	/// from the map.
	#[inline(always)]
	pub fn remove_entry(self) -> ((Namespace<'static>, NcName), V) {
		// XXX: this does not remove the parent map if it now is empty, unlike
		// other removal methods.
		let (name, value) = self.entry.remove_entry();
		((self.namespace, name), value)
	}
}

enum VacantInner<'a, V> {
	OuterVacant {
		entry: btree_map::VacantEntry<'a, Namespace<'static>, BTreeMap<NcName, V>>,
		name: NcName,
	},
	InnerVacant {
		namespace: Namespace<'static>,
		entry: btree_map::VacantEntry<'a, NcName, V>,
	},
}

/// A view into a vacant entry in a `XmlMap`.
///
/// It is part of the [`Entry`] enum.
pub struct VacantEntry<'a, V>(VacantInner<'a, V>);

impl<'a, V> VacantEntry<'a, V> {
	/// Gets a reference to the key that would be used when inserting a value
	/// through the VacantEntry.
	pub fn key(&self) -> (&Namespace<'static>, &NcName) {
		match self.0 {
			VacantInner::OuterVacant {
				ref entry,
				ref name,
			} => (entry.key(), name),
			VacantInner::InnerVacant {
				ref namespace,
				ref entry,
			} => (namespace, entry.key()),
		}
	}

	/// Takes ownership of the key.
	pub fn into_key(self) -> (Namespace<'static>, NcName) {
		match self.0 {
			VacantInner::OuterVacant { entry, name } => (entry.into_key(), name),
			VacantInner::InnerVacant { namespace, entry } => (namespace, entry.into_key()),
		}
	}

	/// Sets the value of the entry with the `VacantEntry`'s attribute, and
	/// returns a mutable reference to its value.
	pub fn insert(self, value: V) -> &'a mut V {
		match self.0 {
			VacantInner::OuterVacant { entry, name } => {
				let map = entry.insert(BTreeMap::new());
				match map.entry(name) {
					btree_map::Entry::Vacant(v) => v.insert(value),
					_ => unreachable!(),
				}
			}
			VacantInner::InnerVacant { entry, .. } => entry.insert(value),
		}
	}
}

/// A view into a single entry in an [`XmlMap`], which may either be vacant
/// or occupied.
///
/// This `enum` is constructed from the [`XmlMap::entry`] method.
pub enum Entry<'a, V> {
	/// A vacant entry.
	Vacant(VacantEntry<'a, V>),

	/// An occupied entry.
	Occupied(OccupiedEntry<'a, V>),
}

/// An owning iterator over the entries of an `XmlMap`.
///
/// This struct is created by the [`into_iter`][`IntoIterator::into_iter`]
/// method on the `XmlMap`.
pub struct IntoIter<V> {
	outer: btree_map::IntoIter<Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<(Namespace<'static>, btree_map::IntoIter<NcName, V>)>,
}

impl<V> IntoIter<V> {
	fn new(mut outer: btree_map::IntoIter<Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = Self::fix_inner(outer.next());
		Self { outer, inner }
	}

	fn fix_inner(
		inner: Option<(Namespace<'static>, BTreeMap<NcName, V>)>,
	) -> Option<(Namespace<'static>, btree_map::IntoIter<NcName, V>)> {
		match inner {
			Some((ns, map)) => Some((ns, map.into_iter())),
			None => None,
		}
	}
}

impl<V> Iterator for IntoIter<V> {
	type Item = ((Namespace<'static>, NcName), V);

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some((namespace, inner)) = self.inner.as_mut() {
				if let Some((name, value)) = inner.next() {
					return Some(((namespace.clone(), name), value));
				}
				self.inner = Self::fix_inner(self.outer.next());
			} else {
				return None;
			}
		}
	}
}

impl<V> IntoIterator for XmlMap<V> {
	type IntoIter = IntoIter<V>;
	type Item = ((Namespace<'static>, NcName), V);

	fn into_iter(self) -> Self::IntoIter {
		IntoIter::new(self.inner.into_iter())
	}
}

/// An iterator over the entries of an `XmlMap`.
///
/// This struct is created by the [`XmlMap::iter`] method.
pub struct Iter<'a, V> {
	outer: btree_map::Iter<'a, Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<(&'a Namespace<'static>, btree_map::Iter<'a, NcName, V>)>,
}

impl<'a, V> Iter<'a, V> {
	fn new(mut outer: btree_map::Iter<'a, Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = outer.next().map(|(ns, inner)| (ns, inner.iter()));
		Self { outer, inner }
	}
}

impl<'a, V> Iterator for Iter<'a, V> {
	type Item = ((&'a Namespace<'static>, &'a NcName), &'a V);

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some((namespace, inner)) = self.inner.as_mut() {
				if let Some((name, value)) = inner.next() {
					return Some(((namespace, name), value));
				}
				self.inner = self.outer.next().map(|(ns, inner)| (ns, inner.iter()));
			} else {
				return None;
			}
		}
	}
}

impl<'a, V> IntoIterator for &'a XmlMap<V> {
	type IntoIter = Iter<'a, V>;
	type Item = ((&'a Namespace<'static>, &'a NcName), &'a V);

	fn into_iter(self) -> Self::IntoIter {
		Iter::new(self.inner.iter())
	}
}

/// A mutable iterator over the entries of an `XmlMap`.
///
/// This struct is created by the [`XmlMap::iter_mut`] method.
pub struct IterMut<'a, V> {
	outer: btree_map::IterMut<'a, Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<(&'a Namespace<'static>, btree_map::IterMut<'a, NcName, V>)>,
}

impl<'a, V> IterMut<'a, V> {
	fn new(mut outer: btree_map::IterMut<'a, Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = outer.next().map(|(ns, inner)| (ns, inner.iter_mut()));
		Self { outer, inner }
	}
}

impl<'a, V> Iterator for IterMut<'a, V> {
	type Item = ((&'a Namespace<'static>, &'a NcName), &'a mut V);

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some((namespace, inner)) = self.inner.as_mut() {
				if let Some((name, value)) = inner.next() {
					return Some(((namespace, name), value));
				}
				self.inner = self.outer.next().map(|(ns, inner)| (ns, inner.iter_mut()));
			} else {
				return None;
			}
		}
	}
}

impl<'a, V> IntoIterator for &'a mut XmlMap<V> {
	type IntoIter = IterMut<'a, V>;
	type Item = ((&'a Namespace<'static>, &'a NcName), &'a mut V);

	fn into_iter(self) -> Self::IntoIter {
		IterMut::new(self.inner.iter_mut())
	}
}

/// A iterator over the attribute namespaces and names of an `XmlMap`.
///
/// This struct is created by the [`XmlMap::names`] method.
pub struct Names<'a, V> {
	outer: btree_map::Iter<'a, Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<(&'a Namespace<'static>, btree_map::Keys<'a, NcName, V>)>,
}

impl<'a, V> Names<'a, V> {
	fn new(mut outer: btree_map::Iter<'a, Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = outer.next().map(|(ns, inner)| (ns, inner.keys()));
		Self { outer, inner }
	}
}

impl<'a, V> Iterator for Names<'a, V> {
	type Item = (&'a Namespace<'static>, &'a NcName);

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some((namespace, inner)) = self.inner.as_mut() {
				if let Some(name) = inner.next() {
					return Some((namespace, name));
				}
				self.inner = self.outer.next().map(|(ns, inner)| (ns, inner.keys()));
			} else {
				return None;
			}
		}
	}
}

/// A iterator over the attribute values of an `XmlMap`.
///
/// This struct is created by the [`XmlMap::values`] method.
pub struct Values<'a, V> {
	outer: btree_map::Values<'a, Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<btree_map::Values<'a, NcName, V>>,
}

impl<'a, V> Values<'a, V> {
	fn new(mut outer: btree_map::Values<'a, Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = outer.next().map(|inner| inner.values());
		Self { outer, inner }
	}
}

impl<'a, V> Iterator for Values<'a, V> {
	type Item = &'a V;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some(inner) = self.inner.as_mut() {
				if let Some(value) = inner.next() {
					return Some(value);
				}
				self.inner = self.outer.next().map(|inner| inner.values());
			} else {
				return None;
			}
		}
	}
}

/// A mutable iterator over the attribute values of an `XmlMap`.
///
/// This struct is created by the [`XmlMap::values_mut`] method.
pub struct ValuesMut<'a, V> {
	outer: btree_map::ValuesMut<'a, Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<btree_map::ValuesMut<'a, NcName, V>>,
}

impl<'a, V> ValuesMut<'a, V> {
	fn new(mut outer: btree_map::ValuesMut<'a, Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = outer.next().map(|inner| inner.values_mut());
		Self { outer, inner }
	}
}

impl<'a, V> Iterator for ValuesMut<'a, V> {
	type Item = &'a mut V;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some(inner) = self.inner.as_mut() {
				if let Some(value) = inner.next() {
					return Some(value);
				}
				self.inner = self.outer.next().map(|inner| inner.values_mut());
			} else {
				return None;
			}
		}
	}
}

/// An owning iterator over the attribute namespaces and names of an
/// `XmlMap`.
///
/// This struct is created by the [`XmlMap::into_names`] method.
pub struct IntoNames<V> {
	outer: btree_map::IntoIter<Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<(Namespace<'static>, btree_map::IntoKeys<NcName, V>)>,
}

impl<V> IntoNames<V> {
	fn new(mut outer: btree_map::IntoIter<Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = Self::fix_inner(outer.next());
		Self { outer, inner }
	}

	fn fix_inner(
		inner: Option<(Namespace<'static>, BTreeMap<NcName, V>)>,
	) -> Option<(Namespace<'static>, btree_map::IntoKeys<NcName, V>)> {
		match inner {
			Some((ns, map)) => Some((ns, map.into_keys())),
			None => None,
		}
	}
}

impl<V> Iterator for IntoNames<V> {
	type Item = (Namespace<'static>, NcName);

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some((namespace, inner)) = self.inner.as_mut() {
				if let Some(name) = inner.next() {
					return Some((namespace.clone(), name));
				}
				self.inner = Self::fix_inner(self.outer.next());
			} else {
				return None;
			}
		}
	}
}

/// An owning iterator over the attribute values of an `XmlMap`.
///
/// This struct is created by the [`XmlMap::into_values`] method.
pub struct IntoValues<V> {
	outer: btree_map::IntoValues<Namespace<'static>, BTreeMap<NcName, V>>,
	inner: Option<btree_map::IntoValues<NcName, V>>,
}

impl<V> IntoValues<V> {
	fn new(mut outer: btree_map::IntoValues<Namespace<'static>, BTreeMap<NcName, V>>) -> Self {
		let inner = outer.next().map(|inner| inner.into_values());
		Self { outer, inner }
	}
}

impl<V> Iterator for IntoValues<V> {
	type Item = V;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some(inner) = self.inner.as_mut() {
				if let Some(value) = inner.next() {
					return Some(value);
				}
				self.inner = self.outer.next().map(|inner| inner.into_values());
			} else {
				return None;
			}
		}
	}
}

impl<V> FromIterator<(Namespace<'static>, NcName, V)> for XmlMap<V> {
	fn from_iter<T>(iter: T) -> Self
	where
		T: IntoIterator<Item = (Namespace<'static>, NcName, V)>,
	{
		let mut result = Self::new();
		result.extend(iter);
		result
	}
}

impl<V> Extend<(Namespace<'static>, NcName, V)> for XmlMap<V> {
	fn extend<T>(&mut self, iter: T)
	where
		T: IntoIterator<Item = (Namespace<'static>, NcName, V)>,
	{
		for (ns, name, v) in iter {
			self.insert(ns, name, v);
		}
	}
}

impl<V> FromIterator<((Namespace<'static>, NcName), V)> for XmlMap<V> {
	fn from_iter<T>(iter: T) -> Self
	where
		T: IntoIterator<Item = ((Namespace<'static>, NcName), V)>,
	{
		let mut result = Self::new();
		result.extend(iter);
		result
	}
}

impl<V> Extend<((Namespace<'static>, NcName), V)> for XmlMap<V> {
	fn extend<T>(&mut self, iter: T)
	where
		T: IntoIterator<Item = ((Namespace<'static>, NcName), V)>,
	{
		for ((ns, name), v) in iter {
			self.insert(ns, name, v);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn xml_map_get_does_not_borrow_namespace_or_name() {
		fn foo<'a>(map: &'a AttrMap, ns: Namespace<'static>, name: NcName) -> Option<&'a String> {
			map.get(&ns, &name)
		}

		let map = AttrMap::new();
		foo(&map, Namespace::NONE, NcName::try_from("foo").unwrap());
	}

	#[test]
	fn xml_map_get_mut_does_not_borrow_namespace_or_name() {
		fn foo<'a>(
			map: &'a mut AttrMap,
			ns: Namespace<'static>,
			name: NcName,
		) -> Option<&'a mut String> {
			map.get_mut(&ns, &name)
		}

		let mut map = AttrMap::new();
		foo(&mut map, Namespace::NONE, NcName::try_from("foo").unwrap());
	}
}
