use char_str::{CharString, ToCharString};
use proptest::{prelude::*, property_test};

#[property_test]
#[cfg_attr(miri, ignore)]
fn create_from_str(input: String) {
    let str = input.as_str();

    let actual = CharString::from(str);
    prop_assert_eq!(&actual, str);
    prop_assert_eq!(actual.len(), str.len());

    if str.len() <= 2 * size_of::<usize>() {
        prop_assert!(!actual.is_heap_allocated());
    } else {
        prop_assert!(actual.is_heap_allocated());
    }
}

#[property_test]
#[cfg_attr(miri, ignore)]
fn create_from_u8_bytes(input: Vec<u8>) {
    let bytes = input.as_slice();

    let actual = CharString::from_utf8(bytes);
    let string = String::from_utf8(bytes.to_vec());
    prop_assert_eq!(actual.is_err(), string.is_err());
    if let (Ok(actual), Ok(string)) = (actual, string) {
        prop_assert_eq!(&actual, &string);
    }

    let actual = CharString::from_utf8_lossy(bytes);
    let string = String::from_utf8_lossy(bytes);
    prop_assert_eq!(&actual, &string);
}

#[property_test]
#[cfg_attr(miri, ignore)]
fn create_from_u16_bytes(input: Vec<u16>) {
    let bytes = input.as_slice();

    let actual = CharString::from_utf16(bytes);
    let string = String::from_utf16(bytes);
    prop_assert_eq!(actual.is_err(), string.is_err());
    if let (Ok(actual), Ok(string)) = (actual, string) {
        prop_assert_eq!(&actual, &string);
    }

    let actual = CharString::from_utf16_lossy(bytes);
    let string = String::from_utf16_lossy(bytes);
    prop_assert_eq!(&actual, &string);
}

#[property_test]
#[cfg_attr(miri, ignore)]
fn collect_from_chars(input: String) {
    let actual = input.chars().collect::<CharString>();
    prop_assert_eq!(&actual, &input);
}

#[property_test]
#[cfg_attr(miri, ignore)]
fn collect_from_strings(input: Vec<String>) {
    let actual = input.clone().into_iter().collect::<CharString>();
    let string = input.into_iter().collect::<String>();
    prop_assert_eq!(&actual, &string);
}

macro_rules! test_integer_to_char_string {
    ($($ty:ty),* $(,)?) => {$(
        paste::paste! {
            #[test]
            fn [<$ty _to_char_string>]() {
                for num in <$ty>::MIN..=<$ty>::MAX {
                    let actual = num.to_char_string();
                    let string = num.to_string();
                    assert_eq!(actual, string);
                }
            }
            #[test]
            fn [<nonzero_ $ty _to_char_string>]() {
                for num in <$ty>::MIN..=<$ty>::MAX {
                    if num == 0 { continue };
                    let num = core::num::NonZero::<$ty>::new(num).unwrap();
                    let actual = num.to_char_string();
                    let string = num.to_string();
                    assert_eq!(actual, string);
                }
            }
        }
    )*};
}
test_integer_to_char_string!(u8, i8);

macro_rules! prop_test_integer_to_char_string {
    ($($ty:ty),* $(,)?) => {$(
        paste::paste! {
            #[property_test]
            #[cfg_attr(miri, ignore)]
            fn [<$ty _to_char_string>](i: $ty) {
                prop_assert_eq!(i.to_char_string(), i.to_string());
            }
            #[property_test]
            #[cfg_attr(miri, ignore)]
            fn [<nonzero_ $ty _to_char_string>](i: core::num::NonZero<$ty>) {
                prop_assert_eq!(i.to_char_string(), i.to_string());
            }
        }
    )*};
}
prop_test_integer_to_char_string!(u16, i16, u32, i32, u64, i64, u128, i128, usize, isize);

macro_rules! test_unsigned_integer_boundaries {
    ($($ty:ty),* $(,)?) => {$(
        paste::paste! {
            #[test]
            fn [<$ty _to_char_string_boundaries>]() {
                let check = |value: $ty| {
                    assert_eq!(value.to_char_string(), value.to_string());
                    if let Some(value) = core::num::NonZero::<$ty>::new(value) {
                        assert_eq!(value.to_char_string(), value.to_string());
                    }
                };

                check(<$ty>::MIN);
                check(<$ty>::MAX);

                let mut power: $ty = 1;
                loop {
                    check(power.saturating_sub(1));
                    check(power);
                    check(power.saturating_add(1));
                    let Some(next) = power.checked_mul(10) else { break };
                    power = next;
                }
            }
        }
    )*};
}

macro_rules! test_signed_integer_boundaries {
    ($($ty:ty),* $(,)?) => {$(
        paste::paste! {
            #[test]
            fn [<$ty _to_char_string_boundaries>]() {
                let check = |value: $ty| {
                    assert_eq!(value.to_char_string(), value.to_string());
                    if let Some(value) = core::num::NonZero::<$ty>::new(value) {
                        assert_eq!(value.to_char_string(), value.to_string());
                    }
                };

                check(<$ty>::MIN);
                check(<$ty>::MAX);

                let mut power: $ty = 1;
                loop {
                    check(power.saturating_sub(1));
                    check(power);
                    check(power.saturating_add(1));

                    let negative = -power;
                    check(negative.saturating_sub(1));
                    check(negative);
                    check(negative.saturating_add(1));

                    let Some(next) = power.checked_mul(10) else { break };
                    power = next;
                }
            }
        }
    )*};
}

test_unsigned_integer_boundaries!(u16, u32, u64, u128, usize);
test_signed_integer_boundaries!(i16, i32, i64, i128, isize);

#[property_test]
#[cfg_attr(miri, ignore)]
fn f32_to_char_string(f: f32) {
    let actual = f.to_char_string();
    let float = actual.parse::<f32>().unwrap();
    prop_assert_eq!(f, float);
}

#[property_test]
#[cfg_attr(miri, ignore)]
fn f64_to_char_string(f: f64) {
    let actual = f.to_char_string();
    let float = actual.parse::<f64>().unwrap();
    prop_assert_eq!(f, float);
}

#[test]
fn bool_to_char_string() {
    let t = true;
    let f = false;
    assert_eq!(t.to_char_string(), t.to_string());
    assert_eq!(f.to_char_string(), f.to_string());
}

#[property_test]
#[cfg_attr(miri, ignore)]
fn char_to_char_string(c: char) {
    prop_assert_eq!(c.to_char_string(), c.to_string());
}

#[property_test]
#[cfg_attr(miri, ignore)]
fn string_to_char_string(s: String) {
    prop_assert_eq!(s.to_char_string(), s);
}
