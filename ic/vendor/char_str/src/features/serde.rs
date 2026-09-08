use crate::{CharStr, CharString};
use core::{fmt, str};
use serde_core::{
    de::{Deserialize, Deserializer, Error, Unexpected, Visitor},
    ser::{Serialize, Serializer},
};

#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
impl Serialize for CharString {
    #[inline]
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_str().serialize(serializer)
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
impl<'de> Deserialize<'de> for CharString {
    #[inline]
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CharStringVisitor;

        impl<'de> Visitor<'de> for CharStringVisitor {
            type Value = CharString;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(CharString::from(v))
            }

            fn visit_borrowed_str<E: Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(CharString::from(v))
            }

            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                CharString::from_utf8(v)
                    .map_err(|_| Error::invalid_value(Unexpected::Bytes(v), &self))
            }

            fn visit_borrowed_bytes<E: Error>(self, v: &'de [u8]) -> Result<Self::Value, E> {
                CharString::from_utf8(v)
                    .map_err(|_| Error::invalid_value(Unexpected::Bytes(v), &self))
            }
        }

        deserializer.deserialize_string(CharStringVisitor)
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
impl Serialize for CharStr {
    #[inline]
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_str().serialize(serializer)
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
impl<'de> Deserialize<'de> for CharStr {
    #[inline]
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CharStrVisitor;

        impl<'de> Visitor<'de> for CharStrVisitor {
            type Value = CharStr;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(CharStr::from(value))
            }

            fn visit_borrowed_str<E: Error>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(CharStr::from(value))
            }

            fn visit_bytes<E: Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                str::from_utf8(value)
                    .map(CharStr::from)
                    .map_err(|_| Error::invalid_value(Unexpected::Bytes(value), &self))
            }

            fn visit_borrowed_bytes<E: Error>(self, value: &'de [u8]) -> Result<Self::Value, E> {
                self.visit_bytes(value)
            }
        }

        deserializer.deserialize_string(CharStrVisitor)
    }
}
