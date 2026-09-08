use std::{
    alloc::{GlobalAlloc, Layout, System},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use char_str::{CharStr, CharString, ReserveError};

struct FailNextAllocation;

static FAIL_NEXT_ALLOCATION: AtomicBool = AtomicBool::new(false);
static FAIL_NEXT_REALLOCATION: AtomicBool = AtomicBool::new(false);

// SAFETY: Allocations are delegated to `System`, except for the single allocation explicitly
// rejected by the test.
unsafe impl GlobalAlloc for FailNextAllocation {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if FAIL_NEXT_ALLOCATION.swap(false, Ordering::SeqCst) {
            ptr::null_mut()
        } else {
            // SAFETY: The caller provides a valid layout.
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if FAIL_NEXT_REALLOCATION.swap(false, Ordering::SeqCst) {
            ptr::null_mut()
        } else {
            // SAFETY: The caller provides a pointer allocated with `layout`, and `new_size` is the
            // requested replacement allocation size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
}

#[global_allocator]
static ALLOCATOR: FailNextAllocation = FailNextAllocation;

#[test]
fn fallible_heap_operations_report_allocation_failure() {
    const TEXT: &str = "a string longer than the inline limit";

    // Explicit heap construction allocates even when the text would fit inline.
    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    let result = CharStr::try_new_heap("short");
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);

    assert_eq!(result, Err(ReserveError));

    // A unique growable allocation is converted to exact storage with `realloc`.
    let mut string = CharString::with_capacity(128);
    string.push_str(TEXT);

    FAIL_NEXT_REALLOCATION.store(true, Ordering::SeqCst);
    let result = string.try_freeze();
    FAIL_NEXT_REALLOCATION.store(false, Ordering::SeqCst);

    assert_eq!(result, Err(ReserveError));

    // A shared growable allocation is copied into a new exact allocation.
    let string = CharString::from(TEXT);
    let growable = string.clone();

    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    let result = string.try_freeze();
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);

    assert_eq!(result, Err(ReserveError));
    assert_eq!(growable, TEXT);

    // A unique exact allocation is converted to growable storage with `realloc`.
    let frozen = CharStr::from(TEXT);

    FAIL_NEXT_REALLOCATION.store(true, Ordering::SeqCst);
    let result = frozen.try_into_char_string();
    FAIL_NEXT_REALLOCATION.store(false, Ordering::SeqCst);

    assert_eq!(result, Err(ReserveError));

    // A shared exact allocation is copied into a new growable allocation.
    let frozen = CharStr::from(TEXT);
    let exact = frozen.clone();

    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    let result = frozen.try_into_char_string();
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);

    assert_eq!(result, Err(ReserveError));
    assert_eq!(exact, TEXT);

    // Joining directly into exact storage reports allocation failure.
    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    let result = CharStr::try_join(&[TEXT, TEXT], ".");
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);

    assert_eq!(result, Err(ReserveError));

    let mut inline = CharString::from("inline");
    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    assert_eq!(inline.try_push_str(TEXT), Err(ReserveError));
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);
    assert_eq!(inline, "inline");

    let mut static_string = CharString::from_static_str(TEXT);
    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    assert_eq!(static_string.try_push('!'), Err(ReserveError));
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);
    assert_eq!(static_string, TEXT);

    let mut unique = CharString::from(TEXT);
    FAIL_NEXT_REALLOCATION.store(true, Ordering::SeqCst);
    assert_eq!(unique.try_push('!'), Err(ReserveError));
    FAIL_NEXT_REALLOCATION.store(false, Ordering::SeqCst);
    assert_eq!(unique, TEXT);

    let mut shared = CharString::from(TEXT);
    let original = shared.clone();
    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    assert_eq!(shared.try_remove(0), Err(ReserveError));
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);
    assert_eq!(shared, TEXT);
    assert_eq!(original, TEXT);

    let mut unique = CharString::with_capacity(128);
    unique.push_str(TEXT);
    FAIL_NEXT_REALLOCATION.store(true, Ordering::SeqCst);
    assert_eq!(unique.try_shrink_to_fit(), Err(ReserveError));
    FAIL_NEXT_REALLOCATION.store(false, Ordering::SeqCst);
    assert_eq!(unique, TEXT);

    let mut shared = CharString::with_capacity(128);
    shared.push_str(TEXT);
    let original = shared.clone();
    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    assert_eq!(shared.try_shrink_to_fit(), Err(ReserveError));
    FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);
    assert_eq!(shared, TEXT);
    assert_eq!(original, TEXT);

    for mut string in [
        CharString::from("inline"),
        CharString::from_static_str(TEXT),
        CharString::from(TEXT),
        original.clone(),
    ] {
        let expected = string.as_str().to_owned();
        assert_eq!(string.try_reserve(usize::MAX), Err(ReserveError));
        assert_eq!(string, expected);
    }
    #[cfg(target_pointer_width = "32")]
    {
        const TEXT: &str = "short";
        const INLINE_LENGTH_LIMIT: usize = (1 << 24) - 2;
        const PREFIXED_CAPACITY: usize = 1 << 24;

        let mut growing = CharString::with_capacity(INLINE_LENGTH_LIMIT);
        growing.push_str(TEXT);
        FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
        assert_eq!(growing.try_reserve(INLINE_LENGTH_LIMIT), Err(ReserveError));
        FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);
        assert_eq!(growing, TEXT);

        let mut shrinking = CharString::with_capacity(PREFIXED_CAPACITY);
        shrinking.push_str(TEXT);
        FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
        assert_eq!(shrinking.try_shrink_to(INLINE_LENGTH_LIMIT), Err(ReserveError));
        FAIL_NEXT_ALLOCATION.store(false, Ordering::SeqCst);
        assert_eq!(shrinking, TEXT);

        let mut freezing = CharString::with_capacity(PREFIXED_CAPACITY);
        freezing.push_str(TEXT);
        FAIL_NEXT_REALLOCATION.store(true, Ordering::SeqCst);
        assert_eq!(freezing.try_freeze(), Err(ReserveError));
        FAIL_NEXT_REALLOCATION.store(false, Ordering::SeqCst);
    }
    // Reserving zero bytes must leave static storage untouched and must not allocate.
    let mut string = CharString::from_static_str(TEXT);
    let static_ptr = string.as_ptr();

    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    FAIL_NEXT_REALLOCATION.store(true, Ordering::SeqCst);
    let result = string.try_reserve(0);
    let allocation_was_not_attempted = FAIL_NEXT_ALLOCATION.swap(false, Ordering::SeqCst);
    let reallocation_was_not_attempted = FAIL_NEXT_REALLOCATION.swap(false, Ordering::SeqCst);

    assert_eq!(result, Ok(()));
    assert!(allocation_was_not_attempted);
    assert!(reallocation_was_not_attempted);
    assert_eq!(string, TEXT);
    assert_eq!(string.as_ptr(), static_ptr);
    assert!(!string.is_heap_allocated());

    string.try_reserve(1).unwrap();
    assert!(string.is_heap_allocated());
    assert_ne!(string.as_ptr(), static_ptr);
    string.push('!');
    assert_eq!(string, "a string longer than the inline limit!");

    // Reserving zero bytes must also preserve a shared heap allocation.
    let mut string = CharString::from(TEXT);
    let shared = string.clone();
    let shared_ptr = string.as_ptr();

    FAIL_NEXT_ALLOCATION.store(true, Ordering::SeqCst);
    FAIL_NEXT_REALLOCATION.store(true, Ordering::SeqCst);
    let result = string.try_reserve(0);
    let allocation_was_not_attempted = FAIL_NEXT_ALLOCATION.swap(false, Ordering::SeqCst);
    let reallocation_was_not_attempted = FAIL_NEXT_REALLOCATION.swap(false, Ordering::SeqCst);

    assert_eq!(result, Ok(()));
    assert!(allocation_was_not_attempted);
    assert!(reallocation_was_not_attempted);
    assert_eq!(string, TEXT);
    assert_eq!(shared, TEXT);
    assert_eq!(string.as_ptr(), shared_ptr);
    assert_eq!(shared.as_ptr(), shared_ptr);

    string.try_reserve(1).unwrap();
    assert_ne!(string.as_ptr(), shared_ptr);
    assert_eq!(shared.as_ptr(), shared_ptr);
    string.push('!');
    assert_eq!(string, "a string longer than the inline limit!");
    assert_eq!(shared, TEXT);
}
