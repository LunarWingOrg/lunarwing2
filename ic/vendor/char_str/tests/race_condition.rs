// MIRIFLAGS="-Zmiri-preemption-rate=1" cargo +nightly miri test --test race_condition
//
// These tests target use-after-free races in CoW uniqueness checks. Loom cannot detect these
// because they are UB (not logic errors). Miri with preemption is the right tool: it explores
// thread interleavings and flags reads/writes to deallocated memory.

use char_str::{CharStr, CharString};
use std::thread;

#[test]
fn drop_while_mutating_thawed_string() {
    let frozen = CharStr::from("a frozen string longer than the inline limit");
    let shared = frozen.clone();

    let th = thread::spawn(move || {
        drop(shared);
    });

    let mut thawed = frozen.into_char_string();
    thawed.push('!');
    assert_eq!(thawed, "a frozen string longer than the inline limit!");

    th.join().unwrap();
}

#[test]
fn drop_while_freezing_string() {
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

#[test]
fn drop_while_push() {
    let mut one = CharString::from("12345678901234567890");
    let two = one.clone();

    let th = thread::spawn(move || {
        drop(two);
    });

    one.push('a');
    assert_eq!(one, "12345678901234567890a");

    th.join().unwrap();
}

#[test]
fn drop_while_remove() {
    let mut one = CharString::from("abcdefghijklmnopqrstuvwxyz");
    let two = one.clone();

    let th = thread::spawn(move || {
        drop(two);
    });

    assert_eq!(one.remove(0), 'a');
    assert_eq!(one, "bcdefghijklmnopqrstuvwxyz");

    th.join().unwrap();
}

#[test]
fn drop_while_retain() {
    let mut one = CharString::from("abcdefghijklmnopqrstuvwxyz");
    let two = one.clone();

    let th = thread::spawn(move || {
        drop(two);
    });

    one.retain(|c| c != 'a');
    assert_eq!(one, "bcdefghijklmnopqrstuvwxyz");

    th.join().unwrap();
}

#[test]
fn drop_while_truncate() {
    let mut one = CharString::from("abcdefghijklmnopqrstuvwxyz");
    let two = one.clone();

    let th = thread::spawn(move || {
        drop(two);
    });

    one.truncate(10);
    assert_eq!(one, "abcdefghij");

    th.join().unwrap();
}
