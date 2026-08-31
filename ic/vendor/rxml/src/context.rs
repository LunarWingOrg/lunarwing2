use alloc::borrow::Cow;
#[cfg(not(feature = "extra-platforms"))]
use alloc::sync::Arc;
use core::fmt;
#[cfg(feature = "extra-platforms")]
use portable_atomic_util::Arc;

#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
use std::sync::{Mutex, Weak};

use crate::strings;

#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
use weak_table;

#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
use alloc::string::String;

#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
type StringWeakSet = weak_table::WeakHashSet<Weak<String>>;

/**
# Shared context for multiple parsers

This context allows parsers to share data. This is useful in cases where many
parsers are used in the same application, and all of them encountering similar
data.

As of writing, the context is only used to share namespace URIs encountered in
XML documents, and only if the `shared_ns` feature is used for building.

Even though the context is internally mutable, it can safely be shared with
an immutable reference between parsers.
*/
#[derive(Default)]
pub struct Context {
	#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
	nss: Mutex<StringWeakSet>,
	// this is included to avoid this struct being empty (and thus publicly
	// constructible) on a build without `shared_ns` enabled
	#[allow(dead_code)]
	phantom: (),
}

impl Context {
	/// Create a new context
	pub fn new() -> Context {
		Context::default()
	}

	/// Intern a piece of text
	///
	/// The given cdata is interned in the context and a refcounted pointer
	/// is returned. When the last reference to that pointer expires, the
	/// string will be lazily removed from the internal storage.
	///
	/// The optimal course is taken depending on whether the Cow is borrowed
	/// or owned.
	///
	/// To force expiry, call [`Context::release_temporaries`], although that
	/// should only rarely be necessary and may be detrimental to performance.
	pub fn intern_namespace<'a, T: Into<Cow<'a, str>>>(
		&self,
		ns: T,
	) -> strings::Namespace<'static> {
		let ns = ns.into();
		if let Some(result) = strings::Namespace::try_share_static(&ns) {
			return result;
		}
		#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
		{
			let mut nss = self.nss.lock().unwrap();
			let ptr = match nss.get(&*ns) {
				Some(ptr) => ptr.clone(),
				None => {
					let ptr = Arc::new(ns.into_owned());
					nss.insert(ptr.clone());
					ptr
				}
			};
			strings::Namespace::from(ptr)
		}
		#[cfg(not(all(feature = "shared_ns", not(feature = "extra-platforms"))))]
		strings::Namespace::from(Arc::new(ns.into_owned()))
	}

	/// Remove all unreferenced strings from storage and shrink the storage to
	/// fit the requirements.
	///
	/// This should rarely be necessary to call. The internal storage will
	/// prefer expiring unused strings over reallocating and will only
	/// reallocate if necessary.
	pub fn release_temporaries(&self) {
		#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
		{
			let mut nss = self.nss.lock().unwrap();
			nss.remove_expired();
			nss.shrink_to_fit();
		}
	}

	/// Return the number of namespace strings interned.
	///
	/// Returns zero if built without `shared_ns`. This count includes strings
	/// which are unreferenced and which would be removed before the next
	/// reallocation.
	pub fn namespaces(&self) -> usize {
		#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
		{
			let nss = self.nss.lock().unwrap();
			nss.len()
		}
		#[cfg(not(all(feature = "shared_ns", not(feature = "extra-platforms"))))]
		0
	}

	/// Return the current capacity for the namespace internation structure
	///
	/// Returns zero if built without `shared_ns`.
	pub fn cdata_capacity(&self) -> usize {
		#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
		{
			let nss = self.nss.lock().unwrap();
			nss.capacity()
		}
		#[cfg(not(all(feature = "shared_ns", not(feature = "extra-platforms"))))]
		0
	}
}

impl fmt::Debug for Context {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let mut f = f.debug_struct("Context");
		f.field("instance", &(self as *const Context));
		#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
		{
			let nss = self.nss.lock().unwrap();
			f.field("nss.capacity()", &nss.capacity())
				.field("nss.length()", &nss.len());
		}
		f.finish()
	}
}

impl fmt::UpperHex for Context {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let mut f = f.debug_set();
		#[cfg(all(feature = "shared_ns", not(feature = "extra-platforms")))]
		{
			let nss = self.nss.lock().unwrap();
			for item in nss.iter() {
				f.entry(&(Arc::strong_count(&item) - 1, &*item));
			}
		}
		f.finish()
	}
}
