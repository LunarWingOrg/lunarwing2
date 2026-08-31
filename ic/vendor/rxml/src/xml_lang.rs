/*!
# Handling of the special `xml:lang` attribute

[XML 1.0 § 2.12](https://www.w3.org/TR/REC-xml/#sec-lang-tag) specifies the
special properties of the `xml:lang` attribute. The attribute specifies the
natural language of the content of the element (including its attributes) it
is applied on. The effects of the `xml:lang` attribute are recursive: child
elements have the same language as the parent element, unless they specify an
`xml:lang` attribute with a different value.

By default, `rxml` does not treat the `xml:lang` attribute in any special
way: the `xml:lang` attribute is passed through to the user just like any
other attribute.

For applications which want to apply XML 1.0 semantics to the `xml:lang`
attribute, the following helper structs are provided:

- [`XmlLangTracker`] can be wrapped around an iterator which yields
  `Result<rxml::Event, _>` items to provide the language at the end of the
  event.

- [`XmlLangStack`] is used under the hood and can be used to implement custom
  variants of [`XmlLangTracker`].

*/

use alloc::vec::Vec;

use crate::strings::CompactString;
use crate::{AttrMap, Event, Namespace};

/// Helper struct to resolve the special `xml:lang` attribute.
///
/// This is a stack of `xml:lang` values.
///
/// See [`xml_lang`][`crate::xml_lang`] for details on the special properties
/// of the `xml:lang` attribute.
///
/// There are two ways to interact with this stack:
///
/// 1. Using [`push_from_attrs`][`Self::push_from_attrs`] and
///    [`pop`][`Self::pop`] directly.
/// 2. Using [`handle_event`][`Self::handle_event`], which calls the
///    appropriate method based on the event passed.
///
/// Mixing the two ways is error-prone and should be avoided.
#[derive(Debug)]
pub struct XmlLangStack {
	stack: Vec<(Option<CompactString>, usize)>,
}

impl XmlLangStack {
	/// Create an empty language stack.
	pub fn new() -> Self {
		Self { stack: Vec::new() }
	}

	/// Extract the `xml:lang` attribute from the given attributes and push
	/// it onto the stack.
	///
	/// If the attributes do not contain an `xml:lang` attribute, the current
	/// top of the stack is reused.
	pub fn push_from_attrs(&mut self, attrs: &AttrMap) {
		if let Some(lang) = attrs.get(Namespace::xml(), "lang") {
			if let Some((Some(existing), count)) = self.stack.last_mut() {
				if existing == lang {
					*count += 1;
					return;
				}
			}
			self.stack.push((Some(lang.into()), 0));
		} else if let Some((_, count)) = self.stack.last_mut() {
			*count += 1;
		} else {
			self.stack.push((None, 0));
		}
	}

	/// Remove the topmost element from the stack.
	///
	/// # Panics
	///
	/// If `pop` is called more times than
	/// [`push_from_attrs`][`Self::push_from_attrs`].
	pub fn pop(&mut self) {
		let Some((_, count)) = self.stack.last_mut() else {
			panic!("pop from empty XmlLangStack");
		};
		match count.checked_sub(1) {
			None => {
				self.stack.pop();
			}
			Some(v) => {
				*count = v;
			}
		}
	}

	/// Modify the stack according to the event.
	///
	/// # Panics
	///
	/// If `handle_event` is called with more [`Event::EndElement`] events
	/// than [`Event::StartElement`].
	pub fn handle_event(&mut self, ev: &Event) {
		match ev {
			Event::StartElement(_, _, attrs) => self.push_from_attrs(attrs),
			Event::EndElement(..) => self.pop(),
			_ => (),
		}
	}

	/// Currently effective `xml:lang` value, according to the stack.
	pub fn current(&self) -> Option<&str> {
		self.stack.last().and_then(|x| x.0.as_deref())
	}
}

/**
Track and provide access to the currently effective `xml:lang` value.

See [`xml_lang`][`crate::xml_lang`] for details on the special properties
of the `xml:lang` attribute.

## Example

The following example demonstrates how the [`XmlLangTracker`] keeps track of
the *effective* value of the `xml:lang` attribute.

*/
#[cfg_attr(
	not(feature = "std"),
	doc = "Because the std feature was not enabled at doc build time, the example cannot be tested.\n\n```ignore\n"
)]
#[cfg_attr(feature = "std", doc = "\n```\n")]
/**
# use std::io::BufReader;
# use rxml::{Event, XmlLangTracker, Reader};
let data = &mut &b"<html xml:lang='en'><body>Hello World!</body></html>"[..];
let reader = BufReader::new(data);
let mut reader = XmlLangTracker::wrap(Reader::new(reader));
if let Some(Ok(Event::StartElement(_, (_, name), _))) = reader.next() {
	assert_eq!(name, "html");
	assert_eq!(reader.language(), Some("en"));
} else {
	panic!();
}
if let Some(Ok(Event::StartElement(_, (_, name), _))) = reader.next() {
	assert_eq!(name, "body");
	assert_eq!(reader.language(), Some("en"));
} else {
	panic!();
}
if let Some(Ok(Event::Text(..))) = reader.next() {
	assert_eq!(reader.language(), Some("en"));
} else {
	panic!();
}
if let Some(Ok(Event::EndElement(..))) = reader.next() {
	assert_eq!(reader.language(), Some("en"));
} else {
	panic!();
}
if let Some(Ok(Event::EndElement(..))) = reader.next() {
	assert!(reader.language().is_none());
} else {
	panic!();
}
```
*/
pub struct XmlLangTracker<I> {
	stack: XmlLangStack,
	inner: I,
}

impl<I: Iterator> XmlLangTracker<I> {
	/// Wrap an existing iterator to track `xml:lang` evolution inside it.
	///
	/// **Note:** Iteration of [`XmlLangTracker`] will panic if `inner` emits
	/// unbalanced [`Event::StartElement`] / [`Event::EndElement`]. This can
	/// only happen if `inner` is not at the start of the document.
	pub fn wrap(inner: I) -> Self {
		Self {
			inner,
			stack: XmlLangStack::new(),
		}
	}

	/// Access the inner iterator.
	///
	/// **Note:** Mutable access to the inner iterator is not provided because
	/// that may lead to incorrect results.
	pub fn inner(&self) -> &I {
		&self.inner
	}

	/// Return the `xml:lang` value effective at the current point of the
	/// iterator.
	pub fn language(&self) -> Option<&str> {
		self.stack.current()
	}
}

impl<E, I: Iterator<Item = Result<Event, E>>> Iterator for XmlLangTracker<I> {
	type Item = I::Item;

	fn next(&mut self) -> Option<Self::Item> {
		let event = self.inner.next()?;
		if let Ok(event) = event.as_ref() {
			self.stack.handle_event(event);
		}
		Some(event)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::borrow::ToOwned;

	#[test]
	fn xml_lang_stack_empty() {
		let stack = XmlLangStack::new();
		assert!(stack.current().is_none());
	}

	#[test]
	#[should_panic = "pop from empty XmlLangStack"]
	fn xml_lang_stack_empty_pop_panics() {
		let mut stack = XmlLangStack::new();
		stack.pop();
	}

	#[test]
	fn xml_lang_stack_from_attrs() {
		let mut attrs = AttrMap::new();
		attrs.insert(Namespace::XML, "lang".try_into().unwrap(), "en".to_owned());

		let mut stack = XmlLangStack::new();
		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("en"));

		stack.pop();
		assert!(stack.current().is_none());
	}

	#[test]
	fn xml_lang_stack_from_attrs_without_xml_lang() {
		let attrs = AttrMap::new();

		let mut stack = XmlLangStack::new();
		stack.push_from_attrs(&attrs);
		assert!(stack.current().is_none());

		stack.pop();
		assert!(stack.current().is_none());
	}

	#[test]
	fn xml_lang_stack_from_attrs_inheritance() {
		let mut attrs = AttrMap::new();
		attrs.insert(Namespace::XML, "lang".try_into().unwrap(), "en".to_owned());

		let mut stack = XmlLangStack::new();
		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("en"));

		attrs.clear();

		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("en"));

		stack.pop();
		assert_eq!(stack.current(), Some("en"));

		stack.pop();
		assert!(stack.current().is_none());
	}

	#[test]
	fn xml_lang_stack_from_attrs_inheritance_with_override() {
		let mut attrs = AttrMap::new();
		attrs.insert(Namespace::XML, "lang".try_into().unwrap(), "en".to_owned());

		let mut stack = XmlLangStack::new();
		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("en"));

		attrs.clear();

		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("en"));

		attrs.insert(Namespace::XML, "lang".try_into().unwrap(), "de".to_owned());

		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("de"));

		attrs.clear();

		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("de"));

		stack.pop();
		assert_eq!(stack.current(), Some("de"));

		stack.pop();
		assert_eq!(stack.current(), Some("en"));

		stack.pop();
		assert_eq!(stack.current(), Some("en"));

		stack.pop();
		assert!(stack.current().is_none());
	}

	#[test]
	fn xml_lang_stack_from_attrs_with_nested_same_value() {
		let mut attrs = AttrMap::new();
		attrs.insert(Namespace::XML, "lang".try_into().unwrap(), "en".to_owned());

		let mut stack = XmlLangStack::new();
		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("en"));

		stack.push_from_attrs(&attrs);
		assert_eq!(stack.current(), Some("en"));

		stack.pop();
		assert_eq!(stack.current(), Some("en"));

		stack.pop();
		assert!(stack.current().is_none());
	}
}
