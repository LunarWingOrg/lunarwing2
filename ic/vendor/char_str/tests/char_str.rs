use core::cell::Cell;

use char_str::{CharStr, CharString, ReserveError, format_char, format_char_str};

const INLINE_LIMIT: usize = CharStr::INLINE_CAPACITY;

#[test]
fn size() {
    assert_eq!(size_of::<CharStr>(), 2 * size_of::<usize>());
    assert_eq!(size_of::<Option<CharStr>>(), size_of::<CharStr>());
    assert_eq!(CharStr::INLINE_CAPACITY, size_of::<CharStr>());
}

#[test]
fn format_macros() {
    let name = "world";
    let inline = format_char!("{name}!");
    let frozen = format_char_str!("{name}!");
    let heap = format_char_str!("a string longer than the inline limit: {name}");

    assert_eq!(inline, "world!");
    assert!(!inline.is_heap_allocated());
    assert_eq!(frozen, "world!");
    assert!(!frozen.is_heap_allocated());
    assert_eq!(heap, "a string longer than the inline limit: world");
    assert!(heap.is_heap_allocated());
}

#[test]
#[should_panic(expected = "a formatting trait implementation returned an error")]
fn format_macro_panics_if_display_fails() {
    struct Fails;

    impl core::fmt::Display for Fails {
        fn fmt(&self, _f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            Err(core::fmt::Error)
        }
    }

    let _ = format_char!("{Fails}");
}

#[test]
fn storage_kinds() {
    let inline = CharStr::from("x".repeat(INLINE_LIMIT));
    let heap = CharStr::from("x".repeat(INLINE_LIMIT + 1));
    const STATIC: CharStr =
        CharStr::from_static_str("a static string longer than the inline limit");

    assert!(!inline.is_heap_allocated());
    assert!(heap.is_heap_allocated());
    assert!(!STATIC.is_heap_allocated());
}

#[test]
fn explicit_storage_constructors() {
    let inline_text = "x".repeat(INLINE_LIMIT);
    let long_text = "x".repeat(INLINE_LIMIT + 1);

    let inline = CharStr::new_inline(&inline_text).unwrap();
    assert_eq!(inline, inline_text);
    assert!(!inline.is_heap_allocated());
    assert!(CharStr::new_inline(&long_text).is_none());

    let heap = CharStr::new_heap("short");
    assert_eq!(heap, "short");
    assert!(heap.is_heap_allocated());

    let fallible_heap = CharStr::try_new_heap("").unwrap();
    assert!(fallible_heap.is_empty());
    assert!(fallible_heap.is_heap_allocated());
}

#[test]
fn clone_shares_heap_storage() {
    let one = CharStr::from("a string longer than the inline limit");
    let two = one.clone();

    assert!(core::ptr::eq(one.as_ptr(), two.as_ptr()));
    assert_eq!(one, two);
    assert_eq!(one.cmp(&two), core::cmp::Ordering::Equal);
}

#[test]
fn comparisons_with_shared_char_string_storage() {
    let frozen = CharStr::from_static_str("a static string longer than the inline limit");
    let thawed = frozen.clone().into_char_string();

    assert!(core::ptr::eq(frozen.as_ptr(), thawed.as_ptr()));
    assert_eq!(frozen, thawed);
    assert_eq!(thawed, frozen);
}

#[test]
fn comparisons_with_shared_storage_respect_length() {
    let frozen = CharStr::from_static_str("a static string longer than the inline limit");
    let mut thawed = frozen.clone().into_char_string();
    thawed.truncate(frozen.len() - 1);

    assert!(core::ptr::eq(frozen.as_ptr(), thawed.as_ptr()));
    assert_ne!(frozen, thawed);
    assert_ne!(thawed, frozen);
}

#[test]
fn comparisons_fall_back_to_content() {
    let one = CharStr::from("a string longer than the inline limit");
    let equal = CharStr::from("a string longer than the inline limit");
    let greater = CharStr::from("b string longer than the inline limit");

    assert!(!core::ptr::eq(one.as_ptr(), equal.as_ptr()));
    assert_eq!(one, equal);
    assert_eq!(one.cmp(&equal), core::cmp::Ordering::Equal);
    assert!(one < greater);
}

#[test]
fn concat_accepts_as_ref_str() {
    let slices = [String::from("prefix"), String::from("suffix")];

    assert_eq!(CharStr::concat(&slices), "prefixsuffix");
    assert_eq!(CharStr::try_concat(&slices).unwrap(), "prefixsuffix");
}

#[test]
fn join_uses_smallest_storage_kind() {
    let empty = CharStr::join::<&str>(&[], ".");
    let inline_text = "x".repeat(INLINE_LIMIT);
    let heap_text = "x".repeat(INLINE_LIMIT + 1);
    let inline = CharStr::join(&[&inline_text[..1], &inline_text[1..]], "");
    let heap = CharStr::try_join(&[&heap_text[..1], &heap_text[1..]], "").unwrap();

    assert!(empty.is_empty());
    assert!(!empty.is_heap_allocated());
    assert_eq!(inline, inline_text);
    assert!(!inline.is_heap_allocated());
    assert_eq!(heap, heap_text);
    assert!(heap.is_heap_allocated());
}

#[test]
fn join_accepts_as_ref_str() {
    let slices = [String::from("package"), String::from("module"), String::from("name")];

    assert_eq!(CharStr::join(&slices, "."), "package.module.name");
}

#[test]
fn join_heap_storage_is_shared() {
    let one = CharStr::join(&["a string", "longer than", "the inline limit"], " ");
    let two = one.clone();

    assert!(core::ptr::eq(one.as_ptr(), two.as_ptr()));
}

#[test]
fn try_join_rejects_inconsistent_as_ref_lengths() {
    struct AlternatingStr {
        first: &'static str,
        second: &'static str,
        calls: Cell<usize>,
    }

    impl AsRef<str> for AlternatingStr {
        fn as_ref(&self) -> &str {
            let call = self.calls.get();
            self.calls.set(call + 1);
            if call == 0 { self.first } else { self.second }
        }
    }

    let grows_after_measurement = [AlternatingStr {
        first: "short",
        second: "a string longer than the inline limit",
        calls: Cell::new(0),
    }];
    let shrinks_after_measurement = [AlternatingStr {
        first: "a string longer than the inline limit",
        second: "short",
        calls: Cell::new(0),
    }];

    assert_eq!(CharStr::try_join(&grows_after_measurement, ""), Err(ReserveError));
    assert_eq!(CharStr::try_join(&shrinks_after_measurement, ""), Err(ReserveError));
}

#[test]
fn try_join_releases_heap_storage_if_as_ref_panics() {
    struct PanicsOnSecondCall(Cell<bool>);

    impl AsRef<str> for PanicsOnSecondCall {
        fn as_ref(&self) -> &str {
            assert!(!self.0.replace(true), "second call");
            "a string longer than the inline limit"
        }
    }

    let slice = [PanicsOnSecondCall(Cell::new(false))];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = CharStr::try_join(&slice, "");
    }));

    assert!(result.is_err());
}

#[test]
fn shared_heap_conversions_copy_storage() {
    let mut string = CharString::with_capacity(128);
    string.push_str("a string longer than the inline limit");
    let growable = string.clone();

    let frozen = string.freeze();
    let shared = frozen.clone();
    let mut thawed = frozen.into_char_string();
    let thawed_ptr = thawed.as_ptr();
    thawed.push_str(" with more text");

    assert!(!core::ptr::eq(growable.as_ptr(), shared.as_ptr()));
    assert!(!core::ptr::eq(shared.as_ptr(), thawed_ptr));
    assert_eq!(growable, "a string longer than the inline limit");
    assert_eq!(shared, "a string longer than the inline limit");
    assert_eq!(thawed, "a string longer than the inline limit with more text");
}

#[test]
fn unique_heap_conversions_preserve_contents() {
    let text = "a string longer than the inline limit";
    let mut string = CharString::with_capacity(128);
    string.push_str(text);

    let frozen = string.try_freeze().unwrap();
    let thawed = frozen.into_char_string();

    assert_eq!(thawed, text);
    assert_eq!(thawed.capacity(), thawed.len());
}

#[test]
fn inline_and_static_conversions_preserve_storage_kind() {
    let inline = CharString::from("short").freeze();
    assert!(!inline.is_heap_allocated());
    assert!(!inline.into_char_string().is_heap_allocated());

    const TEXT: &str = "a static string longer than the inline limit";
    let string = CharString::from_static_str(TEXT);
    let ptr = string.as_ptr();
    let frozen = string.freeze();

    assert!(!frozen.is_heap_allocated());
    assert!(core::ptr::eq(ptr, frozen.as_ptr()));

    let thawed = frozen.into_char_string();
    assert!(!thawed.is_heap_allocated());
    assert!(core::ptr::eq(ptr, thawed.as_ptr()));
}

#[test]
fn heap_conversions_preserve_storage_for_short_contents() {
    let string = CharString::with_capacity(INLINE_LIMIT + 1);
    assert!(string.is_heap_allocated());

    let frozen = string.freeze();
    assert!(frozen.is_heap_allocated());
    assert!(frozen.is_empty());

    let thawed = frozen.into_char_string();
    assert!(thawed.is_heap_allocated());
    assert!(thawed.is_empty());
    assert_eq!(thawed.capacity(), 0);
}

#[test]
fn clear_unique_thawed_string_retains_growable_storage() {
    let mut thawed =
        CharStr::from("a frozen string longer than the inline limit").into_char_string();
    let capacity = thawed.capacity();

    thawed.clear();

    assert!(thawed.is_empty());
    assert_eq!(thawed.capacity(), capacity);
    assert!(thawed.is_heap_allocated());
}

#[test]
fn clear_shared_thawed_string_preserves_frozen_clone() {
    let frozen = CharStr::from("a frozen string longer than the inline limit");
    let shared = frozen.clone();
    let mut thawed = frozen.into_char_string();
    let capacity = thawed.capacity();

    thawed.clear();

    assert!(thawed.is_empty());
    assert_eq!(thawed.capacity(), capacity);
    assert!(thawed.is_heap_allocated());
    assert_eq!(shared, "a frozen string longer than the inline limit");
    assert!(shared.is_heap_allocated());
}

#[test]
fn shrink_to_fit_keeps_growable_heap_storage() {
    let text = "a string longer than the inline limit";
    let mut string = CharString::with_capacity(128);
    string.push_str(text);

    string.shrink_to_fit();
    assert_eq!(string.capacity(), text.len());

    string.clear();
    assert!(string.is_empty());
    assert_eq!(string.capacity(), text.len());
    assert!(string.is_heap_allocated());
}

#[test]
fn collect_freezes_builder() {
    let frozen: CharStr = "a string longer than the inline limit".chars().collect();

    assert_eq!(frozen, "a string longer than the inline limit");
    assert!(frozen.is_heap_allocated());
}
