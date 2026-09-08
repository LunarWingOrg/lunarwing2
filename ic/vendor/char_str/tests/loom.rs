// RUSTFLAGS="--cfg loom" cargo test --test loom --release --features loom -- --test-threads=1
#![cfg(loom)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};

use char_str::{CharStr, CharString};
use loom::thread;

// Avoid matching the sizes of Loom's internal allocations.
const EXACT_TEXT_LEN: usize = 4099;
const COW_TEXT_LEN: usize = 2053;
const COW_CAPACITY: usize = 8209;
const COW_DETACHED_CAPACITY: usize = COW_TEXT_LEN * 3 / 2;
const CONVERSION_TEXT_LEN: usize = 6151;
const CONVERSION_CAPACITY: usize = 12_347;

const EXACT_HEADER_SIZE: usize = size_of::<loom::sync::atomic::AtomicUsize>();
const GROWABLE_HEADER_SIZE: usize = EXACT_HEADER_SIZE + size_of::<usize>();
const EXACT_ALLOCATION_SIZE: usize = EXACT_HEADER_SIZE + EXACT_TEXT_LEN;
const COW_ALLOCATION_SIZE: usize = GROWABLE_HEADER_SIZE + COW_CAPACITY;
const COW_DETACHED_ALLOCATION_SIZE: usize = GROWABLE_HEADER_SIZE + COW_DETACHED_CAPACITY;
const CONVERSION_ALLOCATION_SIZE: usize = GROWABLE_HEADER_SIZE + CONVERSION_CAPACITY;
const CONVERSION_EXACT_ALLOCATION_SIZE: usize = EXACT_HEADER_SIZE + CONVERSION_TEXT_LEN;
const CONVERSION_THAWED_ALLOCATION_SIZE: usize = GROWABLE_HEADER_SIZE + CONVERSION_TEXT_LEN;

const _: () = {
    let sizes = [
        EXACT_ALLOCATION_SIZE,
        COW_ALLOCATION_SIZE,
        COW_DETACHED_ALLOCATION_SIZE,
        CONVERSION_ALLOCATION_SIZE,
        CONVERSION_EXACT_ALLOCATION_SIZE,
        CONVERSION_THAWED_ALLOCATION_SIZE,
    ];
    let mut first = 0;
    while first < sizes.len() {
        let mut second = first + 1;
        while second < sizes.len() {
            assert!(sizes[first] != sizes[second]);
            second += 1;
        }
        first += 1;
    }
};

struct AllocationTracker {
    size: usize,
    seen: AtomicBool,
    live: AtomicIsize,
}

impl AllocationTracker {
    const fn new(size: usize) -> Self {
        Self { size, seen: AtomicBool::new(false), live: AtomicIsize::new(0) }
    }
}

static TRACKERS: [AllocationTracker; 6] = [
    AllocationTracker::new(EXACT_ALLOCATION_SIZE),
    AllocationTracker::new(COW_ALLOCATION_SIZE),
    AllocationTracker::new(COW_DETACHED_ALLOCATION_SIZE),
    AllocationTracker::new(CONVERSION_ALLOCATION_SIZE),
    AllocationTracker::new(CONVERSION_EXACT_ALLOCATION_SIZE),
    AllocationTracker::new(CONVERSION_THAWED_ALLOCATION_SIZE),
];

struct TrackingAllocator;

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);

fn tracker(size: usize) -> Option<&'static AllocationTracker> {
    TRACKERS.iter().find(|tracker| tracker.size == size)
}

fn track_allocation(size: usize) {
    if let Some(tracker) = tracker(size) {
        tracker.seen.store(true, Relaxed);
        tracker.live.fetch_add(1, Relaxed);
    }
}

fn track_deallocation(size: usize) {
    if let Some(tracker) = tracker(size) {
        tracker.live.fetch_sub(1, Relaxed);
    }
}

fn start_tracking() {
    for tracker in &TRACKERS {
        tracker.seen.store(false, Relaxed);
        tracker.live.store(0, Relaxed);
    }
    TRACK_ALLOCATIONS.store(true, Relaxed);
}

fn stop_tracking(expected_sizes: &[usize]) {
    TRACK_ALLOCATIONS.store(false, Relaxed);

    for tracker in &TRACKERS {
        assert_eq!(tracker.live.load(Relaxed), 0, "allocation size {} was leaked", tracker.size);
    }
    for size in expected_sizes {
        assert!(tracker(*size).unwrap().seen.load(Relaxed), "allocation size {size} was not seen");
    }
}

// SAFETY: The allocator forwards valid layouts unchanged while tracking crate-specific heap
// buffers.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The layout is forwarded unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && TRACK_ALLOCATIONS.load(Relaxed) {
            track_allocation(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACK_ALLOCATIONS.load(Relaxed) {
            track_deallocation(layout.size());
        }
        // SAFETY: The pointer and layout are forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The pointer, layout, and new size are forwarded unchanged to the system
        // allocator.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() && TRACK_ALLOCATIONS.load(Relaxed) {
            track_deallocation(layout.size());
            track_allocation(new_size);
        }
        new_ptr
    }
}

macro_rules! loom_test {
    (fn $name:ident() { $($tt:tt)* }) => {
        #[test]
        fn $name() {
            loom::model(|| {
                $($tt)*
            });
        }
    };
}

#[test]
fn concurrent_exact_clone_and_drop() {
    loom::model(|| {
        let text = "a".repeat(EXACT_TEXT_LEN);
        start_tracking();
        {
            let original = CharStr::from(text.as_str());
            let shared = original.clone();

            let thread = thread::spawn(move || {
                let clone = shared.clone();
                assert_eq!(clone.len(), EXACT_TEXT_LEN);
                assert!(clone.bytes().all(|byte| byte == b'a'));
            });

            let clone = original.clone();
            assert_eq!(clone.len(), EXACT_TEXT_LEN);
            assert!(clone.bytes().all(|byte| byte == b'a'));
            thread.join().unwrap();
        }

        stop_tracking(&[EXACT_ALLOCATION_SIZE]);
    });
}

#[test]
fn concurrent_growable_cow_and_drop() {
    loom::model(|| {
        let text = "a".repeat(COW_TEXT_LEN);
        start_tracking();
        {
            let mut original = CharString::with_capacity(COW_CAPACITY);
            original.push_str(&text);
            let shared = original.clone();

            let thread = thread::spawn(move || {
                let mut detached = shared.clone();
                detached.push('b');
                assert_eq!(shared.len(), COW_TEXT_LEN);
                assert_eq!(detached.len(), COW_TEXT_LEN + 1);
            });

            original.push('c');
            assert_eq!(original.len(), COW_TEXT_LEN + 1);
            thread.join().unwrap();
        }

        stop_tracking(&[COW_ALLOCATION_SIZE, COW_DETACHED_ALLOCATION_SIZE]);
    });
}

#[test]
fn concurrent_freeze_thaw_and_drop() {
    loom::model(|| {
        let text = "a".repeat(CONVERSION_TEXT_LEN);
        start_tracking();
        {
            let mut growable = CharString::with_capacity(CONVERSION_CAPACITY);
            growable.push_str(&text);
            let shared_growable = growable.clone();

            let thread = thread::spawn(move || drop(shared_growable));
            let frozen = growable.freeze();
            assert_eq!(frozen.len(), CONVERSION_TEXT_LEN);
            thread.join().unwrap();

            let shared_frozen = frozen.clone();
            let thread = thread::spawn(move || drop(shared_frozen));
            let thawed = frozen.into_char_string();
            assert_eq!(thawed.len(), CONVERSION_TEXT_LEN);
            thread.join().unwrap();
        }

        stop_tracking(&[
            CONVERSION_ALLOCATION_SIZE,
            CONVERSION_EXACT_ALLOCATION_SIZE,
            CONVERSION_THAWED_ALLOCATION_SIZE,
        ]);
    });
}

loom_test! {
    fn concurrent_frozen_clone_and_thaw() {
        let frozen = CharStr::from("a frozen string longer than the inline limit");
        let shared = frozen.clone();

        let th = thread::spawn(move || {
            let clone = shared.clone();
            assert_eq!(clone, "a frozen string longer than the inline limit");
        });

        let mut thawed = frozen.into_char_string();
        thawed.push('!');
        assert_eq!(thawed, "a frozen string longer than the inline limit!");

        th.join().unwrap();
    }
}

loom_test! {
    fn concurrent_drop_and_thaw() {
        let frozen = CharStr::from("a frozen string longer than the inline limit");
        let shared = frozen.clone();

        let th = thread::spawn(move || {
            drop(shared);
        });

        let thawed = frozen.into_char_string();
        assert_eq!(thawed, "a frozen string longer than the inline limit");

        th.join().unwrap();
    }
}

loom_test! {
    fn concurrent_drop_and_freeze() {
        let mut string = CharString::with_capacity(128);
        string.push_str("a string longer than the inline limit");
        let shared = string.clone();

        let th = thread::spawn(move || {
            drop(shared);
        });

        let frozen = string.freeze();
        assert_eq!(frozen, "a string longer than the inline limit");

        th.join().unwrap();
    }
}

loom_test! {
    fn concurrent_push() {
        let mut one = CharString::from("12345678901234567890");
        let two = one.clone();

        let th = thread::spawn(move || {
            let mut three = two.clone();
            three.push('a');
            assert_eq!(two, "12345678901234567890");
            assert_eq!(three, "12345678901234567890a");
        });

        one.push('a');
        assert_eq!(one, "12345678901234567890a");

        th.join().unwrap();
    }
}

loom_test! {
    fn concurrent_remove() {
        let mut one = CharString::from("abcdefghijklmnopqrstuvwxyz");
        let two = one.clone();

        let th = thread::spawn(move || {
            let mut three = two.clone();
            assert_eq!(three.remove(3), 'd');
            assert_eq!(two, "abcdefghijklmnopqrstuvwxyz");
            assert_eq!(three, "abcefghijklmnopqrstuvwxyz");
        });

        assert_eq!(one.remove(3), 'd');
        assert_eq!(one, "abcefghijklmnopqrstuvwxyz");

        th.join().unwrap();
    }
}
