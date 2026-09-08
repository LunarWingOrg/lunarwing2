use crate::{CharString, ToCharStringError, UnwrapWithMsg, repr::Repr};
use alloc::string::String;
use castaway::{LifetimeFree, match_type};
use core::{fmt, fmt::Write, num::NonZero};

/// A trait for converting a value to a [`CharString`].
pub trait ToCharString {
    /// Converts the value to a [`CharString`].
    ///
    /// # Panics
    ///
    /// Panics if conversion fails. For a non-panicking version, use [`try_to_char_string`].
    ///
    /// [`try_to_char_string`]: Self::try_to_char_string
    fn to_char_string(&self) -> CharString {
        self.try_to_char_string().unwrap_with_msg()
    }

    /// Attempts to convert the value to a [`CharString`].
    ///
    /// # Errors
    ///
    /// Returns a [`ToCharStringError`] if the conversion fails.
    fn try_to_char_string(&self) -> Result<CharString, ToCharStringError>;
}

// NOTE: the restriction of `castaway` is `T` must be Sized.
impl<T: fmt::Display> ToCharString for T {
    fn try_to_char_string(&self) -> Result<CharString, ToCharStringError> {
        let repr = match_type!(self, {
            &i8 as s => Repr::from_num(*s)?,
            &u8 as s => Repr::from_num(*s)?,
            &i16 as s => Repr::from_num(*s)?,
            &u16 as s => Repr::from_num(*s)?,
            &i32 as s => Repr::from_num(*s)?,
            &u32 as s => Repr::from_num(*s)?,
            &i64 as s => Repr::from_num(*s)?,
            &u64 as s => Repr::from_num(*s)?,
            &i128 as s => Repr::from_num(*s)?,
            &u128 as s => Repr::from_num(*s)?,
            &isize as s => Repr::from_num(*s)?,
            &usize as s => Repr::from_num(*s)?,

            &NonZero<i8> as s => Repr::from_num(*s)?,
            &NonZero<u8> as s => Repr::from_num(*s)?,
            &NonZero<i16> as s => Repr::from_num(*s)?,
            &NonZero<u16> as s => Repr::from_num(*s)?,
            &NonZero<i32> as s => Repr::from_num(*s)?,
            &NonZero<u32> as s => Repr::from_num(*s)?,
            &NonZero<i64> as s => Repr::from_num(*s)?,
            &NonZero<u64> as s => Repr::from_num(*s)?,
            &NonZero<i128> as s => Repr::from_num(*s)?,
            &NonZero<u128> as s => Repr::from_num(*s)?,
            &NonZero<isize> as s => Repr::from_num(*s)?,
            &NonZero<usize> as s => Repr::from_num(*s)?,

            &f32 as s => Repr::from_num(*s)?,
            &f64 as s => Repr::from_num(*s)?,

            &bool as s => Repr::from_bool(*s),
            &char as s => Repr::from_char(*s),

            &String as s => Repr::from_str(s.as_str())?,
            &CharString as s => return Ok(s.clone()),

            s => {
                let mut buf = CharString::new();
                write!(buf, "{}", s)?;
                return Ok(buf)
            }
        });
        Ok(CharString(repr))
    }
}

// SAFETY:
// - `CharString` is `'static`.
// - `CharString` does not contain any lifetime parameter.
// These two conditions are also applied to `Repr` which is the only field of `CharString`.
unsafe impl LifetimeFree for CharString {}
unsafe impl LifetimeFree for Repr {}
