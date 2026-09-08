#![cfg(feature = "salsa")]

use char_str::{CharStr, CharString};
use salsa::Update;

#[test]
fn char_string_keeps_equal_values_and_replaces_changed_values() {
    let mut old = CharString::with_capacity(128);
    old.push_str("same");
    let old_pointer = old.as_ptr();
    let old_capacity = old.capacity();

    // SAFETY: `old` is valid and exclusively borrowed for the duration of the update.
    let changed = unsafe { CharString::maybe_update(&mut old, CharString::from("same")) };
    assert!(!changed);
    assert_eq!(old.as_ptr(), old_pointer);
    assert_eq!(old.capacity(), old_capacity);

    // SAFETY: `old` is valid and exclusively borrowed for the duration of the update.
    let changed = unsafe { CharString::maybe_update(&mut old, CharString::from("different")) };
    assert!(changed);
    assert_eq!(old, "different");
}

#[test]
fn char_str_keeps_equal_values_and_replaces_changed_values() {
    let mut old = CharStr::from("a string longer than the inline limit");
    let old_pointer = old.as_ptr();

    // SAFETY: `old` is valid and exclusively borrowed for the duration of the update.
    let changed = unsafe {
        CharStr::maybe_update(&mut old, CharStr::from("a string longer than the inline limit"))
    };
    assert!(!changed);
    assert_eq!(old.as_ptr(), old_pointer);

    // SAFETY: `old` is valid and exclusively borrowed for the duration of the update.
    let changed =
        unsafe { CharStr::maybe_update(&mut old, CharStr::from("a different long string")) };
    assert!(changed);
    assert_eq!(old, "a different long string");
}
