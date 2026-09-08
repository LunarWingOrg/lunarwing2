#![cfg(feature = "get-size")]

use core::mem::size_of;

use char_str::{CharStr, CharString};
use get_size2::{GetSize, StandardTracker};

#[test]
fn inline_and_static_strings_have_no_heap_size() {
    assert_eq!(CharString::from("inline").get_heap_size(), 0);
    assert_eq!(CharStr::from("inline").get_heap_size(), 0);

    const STATIC: CharStr =
        CharStr::from_static_str("a static string longer than the inline limit");
    assert_eq!(STATIC.get_heap_size(), 0);
}

#[test]
fn exact_heap_storage_is_counted_once() {
    let value = CharStr::from("a string longer than the inline limit");
    let shared = value.clone();
    let expected = size_of::<usize>() + value.len();

    let (first, tracker) = value.get_heap_size_with_tracker(StandardTracker::new());
    let (second, _) = shared.get_heap_size_with_tracker(tracker);

    assert_eq!(first, expected);
    assert_eq!(second, 0);
}

#[test]
fn growable_heap_storage_counts_capacity_once() {
    let value = CharString::with_capacity(128);
    let shared = value.clone();
    let expected = 2 * size_of::<usize>() + value.capacity();

    let (first, tracker) = value.get_heap_size_with_tracker(StandardTracker::new());
    let (second, _) = shared.get_heap_size_with_tracker(tracker);

    assert_eq!(first, expected);
    assert_eq!(second, 0);
}
