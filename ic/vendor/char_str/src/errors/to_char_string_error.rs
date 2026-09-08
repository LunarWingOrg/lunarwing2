use core::{error::Error, fmt};

use super::ReserveError;

/// An error that can occur when converting a value to a [`CharString`].
///
/// This error can be caused by either a reserve error when allocating memory,
/// or a formatting error when converting the value to a string.
///
/// [`CharString`]: crate::CharString
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToCharStringError {
    /// An error occurred while trying to allocate memory.
    Reserve(ReserveError),
    /// A formatting error occurred during conversion.
    Fmt(fmt::Error),
}

impl Error for ToCharStringError {}

impl fmt::Display for ToCharStringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToCharStringError::Reserve(e) => e.fmt(f),
            ToCharStringError::Fmt(e) => e.fmt(f),
        }
    }
}

impl From<ReserveError> for ToCharStringError {
    fn from(value: ReserveError) -> Self {
        ToCharStringError::Reserve(value)
    }
}

impl From<fmt::Error> for ToCharStringError {
    fn from(value: fmt::Error) -> Self {
        ToCharStringError::Fmt(value)
    }
}
