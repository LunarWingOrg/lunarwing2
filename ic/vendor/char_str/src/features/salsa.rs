use crate::{CharStr, CharString};

// SAFETY: `CharString` owns all of its data and contains no borrowed state. Equal values can keep
// the allocation from the previous revision; unequal values are fully replaced.
#[cfg_attr(docsrs, doc(cfg(feature = "salsa")))]
unsafe impl salsa::Update for CharString {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        // SAFETY: Salsa guarantees that `old_pointer` is valid and uniquely available for the
        // duration of this call.
        let old_value = unsafe { &mut *old_pointer };
        if *old_value == new_value {
            false
        } else {
            *old_value = new_value;
            true
        }
    }
}

// SAFETY: `CharStr` owns all of its data and contains no borrowed state. Equal values can keep the
// allocation from the previous revision; unequal values are fully replaced.
#[cfg_attr(docsrs, doc(cfg(feature = "salsa")))]
unsafe impl salsa::Update for CharStr {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        // SAFETY: Salsa guarantees that `old_pointer` is valid and uniquely available for the
        // duration of this call.
        let old_value = unsafe { &mut *old_pointer };
        if *old_value == new_value {
            false
        } else {
            *old_value = new_value;
            true
        }
    }
}
