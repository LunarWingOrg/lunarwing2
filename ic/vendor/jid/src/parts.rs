use alloc::borrow::{Cow, ToOwned};
use alloc::string::{String, ToString};
use core::borrow::Borrow;
use core::fmt;
use core::mem;
use core::ops::Deref;
use core::str::FromStr;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{BareJid, Error, Jid};
use crate::{domain_check, node_check, resource_check};

macro_rules! def_part_parse_doc {
    ($name:ident, $other:ident, $more:expr) => {
        concat!(
            "Parse a [`",
            stringify!($name),
            "`] from a `",
            stringify!($other),
            "`, copying its contents.\n",
            "\n",
            "If the given `",
            stringify!($other),
            "` does not conform to the restrictions imposed by `",
            stringify!($name),
            "`, an error is returned.\n",
            $more,
        )
    };
}

macro_rules! def_part_into_inner_doc {
    ($name:ident, $other:ident, $more:expr) => {
        concat!(
            "Consume the `",
            stringify!($name),
            "` and return the inner `",
            stringify!($other),
            "`.\n",
            $more,
        )
    };
}

#[cfg(feature = "serde")]
#[derive(Deserialize)]
struct NodeDeserializer<'a>(Cow<'a, str>);

#[cfg(feature = "serde")]
impl TryFrom<NodeDeserializer<'_>> for NodePart {
    type Error = Error;

    fn try_from(deserializer: NodeDeserializer) -> Result<NodePart, Self::Error> {
        Ok(NodePart::new(&deserializer.0)?.into_owned())
    }
}

#[cfg(feature = "serde")]
#[derive(Deserialize)]
struct DomainDeserializer<'a>(Cow<'a, str>);

#[cfg(feature = "serde")]
impl TryFrom<DomainDeserializer<'_>> for DomainPart {
    type Error = Error;

    fn try_from(deserializer: DomainDeserializer) -> Result<DomainPart, Self::Error> {
        Ok(DomainPart::new(&deserializer.0)?.into_owned())
    }
}

#[cfg(feature = "serde")]
#[derive(Deserialize)]
struct ResourceDeserializer<'a>(Cow<'a, str>);

#[cfg(feature = "serde")]
impl TryFrom<ResourceDeserializer<'_>> for ResourcePart {
    type Error = Error;

    fn try_from(deserializer: ResourceDeserializer) -> Result<ResourcePart, Self::Error> {
        Ok(ResourcePart::new(&deserializer.0)?.into_owned())
    }
}

macro_rules! def_part_types {
    (
        $(#[$mainmeta:meta])*
        pub struct $name:ident(String) use $check_fn:ident();

        $(#[$refmeta:meta])*
        pub struct ref $borrowed:ident(str);
    ) => {
        $(#[$mainmeta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(transparent)]
        pub struct $name(pub(crate) String);

        impl $name {
            #[doc = def_part_parse_doc!($name, str, "Depending on whether the contents are changed by normalisation operations, this function either returns a copy or a reference to the original data.")]
            #[allow(clippy::new_ret_no_self)]
            pub fn new(s: &str) -> Result<Cow<'_, $borrowed>, Error> {
                let part = $check_fn(s)?;
                match part {
                    Cow::Borrowed(v) => Ok(Cow::Borrowed($borrowed::from_str_unchecked(v))),
                    Cow::Owned(v) => Ok(Cow::Owned(Self(v))),
                }
            }

            #[doc = def_part_into_inner_doc!($name, String, "")]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(s: &str) -> Result<Self, Error> {
                Ok(Self::new(s)?.into_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                <$borrowed as fmt::Display>::fmt(Borrow::<$borrowed>::borrow(self), f)
            }
        }

        impl Deref for $name {
            type Target = $borrowed;

            fn deref(&self) -> &Self::Target {
                Borrow::<$borrowed>::borrow(self)
            }
        }

        impl AsRef<$borrowed> for $name {
            fn as_ref(&self) -> &$borrowed {
                Borrow::<$borrowed>::borrow(self)
            }
        }

        impl AsRef<String> for $name {
            fn as_ref(&self) -> &String {
                &self.0
            }
        }

        impl Borrow<$borrowed> for $name {
            fn borrow(&self) -> &$borrowed {
                $borrowed::from_str_unchecked(self.0.as_str())
            }
        }

        // useful for use in hashmaps
        impl Borrow<String> for $name {
            fn borrow(&self) -> &String {
                &self.0
            }
        }

        // useful for use in hashmaps
        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.0.as_str()
            }
        }

        impl<'x> TryFrom<&'x str> for $name {
            type Error = Error;

            fn try_from(s: &str) -> Result<Self, Error> {
                Self::from_str(s)
            }
        }

        impl From<&$borrowed> for $name {
            fn from(other: &$borrowed) -> Self {
                other.to_owned()
            }
        }

        impl<'x> From<Cow<'x, $borrowed>> for $name {
            fn from(other: Cow<'x, $borrowed>) -> Self {
                other.into_owned()
            }
        }

        $(#[$refmeta])*
        #[repr(transparent)]
        #[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $borrowed(pub(crate) str);

        impl $borrowed {
            pub(crate) fn from_str_unchecked(s: &str) -> &Self {
                // SAFETY: repr(transparent) thing can be transmuted to/from
                // its inner.
                unsafe { mem::transmute(s) }
            }

            /// Access the contents as [`str`] slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Deref for $borrowed {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl ToOwned for $borrowed {
            type Owned = $name;

            fn to_owned(&self) -> Self::Owned {
                $name(self.0.to_string())
            }
        }

        impl AsRef<str> for $borrowed {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $borrowed {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", &self.0)
            }
        }
    }
}

def_part_types! {
    /// The [`NodePart`] is the optional part before the (optional) `@` in any
    /// [`Jid`][crate::Jid], whether [`BareJid`][crate::BareJid] or
    /// [`FullJid`][crate::FullJid].
    ///
    /// The corresponding slice type is [`NodeRef`].
    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    #[cfg_attr(feature = "serde", serde(try_from = "NodeDeserializer"))]
    pub struct NodePart(String) use node_check();

    /// `str`-like type which conforms to the requirements of [`NodePart`].
    ///
    /// See [`NodePart`] for details.
    #[cfg_attr(feature = "serde", derive(Serialize))]
    pub struct ref NodeRef(str);
}

def_part_types! {
    /// The [`DomainPart`] is the part between the (optional) `@` and the
    /// (optional) `/` in any [`Jid`][crate::Jid], whether
    /// [`BareJid`][crate::BareJid] or [`FullJid`][crate::FullJid].
    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    #[cfg_attr(feature = "serde", serde(try_from = "DomainDeserializer"))]
    pub struct DomainPart(String) use domain_check();

    /// `str`-like type which conforms to the requirements of [`DomainPart`].
    ///
    /// See [`DomainPart`] for details.
    #[cfg_attr(feature = "serde", derive(Serialize))]
    pub struct ref DomainRef(str);
}

def_part_types! {
    /// The [`ResourcePart`] is the optional part after the `/` in a
    /// [`Jid`][crate::Jid]. It is mandatory in [`FullJid`][crate::FullJid].
    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    #[cfg_attr(feature = "serde", serde(try_from = "ResourceDeserializer"))]
    pub struct ResourcePart(String) use resource_check();

    /// `str`-like type which conforms to the requirements of
    /// [`ResourcePart`].
    ///
    /// See [`ResourcePart`] for details.
    #[cfg_attr(feature = "serde", derive(Serialize))]
    pub struct ref ResourceRef(str);
}

impl DomainRef {
    /// Construct a bare JID (a JID without a resource) from this domain and
    /// the given node (local part).
    pub fn with_node(&self, node: &NodeRef) -> BareJid {
        BareJid::from_parts(Some(node), self)
    }

    /// Construct a bare JID (a JID without a resource) from this domain and
    /// the given node (local part), by specifying a stringy `node`.
    /// If your node has already been parsed into a [`NodePart`], use [`DomainRef::with_node`].
    pub fn with_node_str(&self, node: &str) -> Result<BareJid, Error> {
        let node = NodePart::new(node)?;
        Ok(self.with_node(&node))
    }
}

impl From<DomainPart> for BareJid {
    fn from(other: DomainPart) -> Self {
        BareJid {
            inner: other.into(),
        }
    }
}

impl From<DomainPart> for Jid {
    fn from(other: DomainPart) -> Self {
        Jid {
            normalized: other.0,
            at: None,
            slash: None,
        }
    }
}

impl<'x> From<&'x DomainRef> for BareJid {
    fn from(other: &'x DomainRef) -> Self {
        Self::from_parts(None, other)
    }
}

impl NodeRef {
    /// Construct a bare JID (a JID without a resource) from this node (the
    /// local part) and the given domain.
    pub fn with_domain(&self, domain: &DomainRef) -> BareJid {
        BareJid::from_parts(Some(self), domain)
    }

    /// Implements XEP-0106 unescape algorithm.
    pub fn unescape(&self) -> Result<Cow<'_, str>, Error> {
        fn hex_to_char(bytes: [u8; 2]) -> Result<char, ()> {
            Ok(match &[bytes[0], bytes[1]] {
                b"20" => ' ',
                b"22" => '"',
                b"26" => '&',
                b"27" => '\'',
                b"2f" => '/',
                b"3a" => ':',
                b"3c" => '<',
                b"3e" => '>',
                b"40" => '@',
                b"5c" => '\\',
                _ => return Err(()),
            })
        }

        let bytes = self.0.as_bytes();
        let mut iter = memchr::memchr_iter(b'\\', bytes).peekable();

        // Fast path in case there is no character to unescape.
        if iter.peek().is_none() {
            return Ok(Cow::Borrowed(self));
        }

        let mut buf = String::with_capacity(bytes.len());
        let mut valid_up_to = 0;
        for index in iter {
            buf.push_str(&self.0[valid_up_to..index]);
            match hex_to_char([bytes[index + 1], bytes[index + 2]]) {
                Ok(char) => {
                    buf.push(char);
                    valid_up_to = index + 3;
                }
                // If the escape code wasn’t valid, we want to copy it as is.
                Err(()) => valid_up_to = index,
            }
        }
        buf.push_str(&self.0[valid_up_to..]);

        if buf.starts_with(' ') || buf.ends_with(' ') {
            return Err(Error::NodePrep);
        }

        Ok(Cow::Owned(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodepart_comparison() {
        let n1 = NodePart::new("foo").unwrap();
        let n2 = NodePart::new("bar").unwrap();
        let n3 = NodePart::new("foo").unwrap();
        assert_eq!(n1, n3);
        assert_ne!(n1, n2);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn nodepart_serde() {
        serde_test::assert_de_tokens(
            &NodePart(String::from("test")),
            &[
                serde_test::Token::TupleStruct {
                    name: "NodePart",
                    len: 1,
                },
                serde_test::Token::BorrowedStr("test"),
                serde_test::Token::TupleStructEnd,
            ],
        );

        serde_test::assert_de_tokens(
            &NodePart(String::from("test")),
            &[
                serde_test::Token::TupleStruct {
                    name: "NodePart",
                    len: 1,
                },
                serde_test::Token::String("test"),
                serde_test::Token::TupleStructEnd,
            ],
        );

        serde_test::assert_de_tokens_error::<NodePart>(
            &[
                serde_test::Token::TupleStruct {
                    name: "NodePart",
                    len: 1,
                },
                serde_test::Token::BorrowedStr("invalid@domain"),
                serde_test::Token::TupleStructEnd,
            ],
            "localpart doesn’t pass nodeprep validation",
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn domainpart_serde() {
        serde_test::assert_de_tokens(
            &DomainPart(String::from("[::1]")),
            &[
                serde_test::Token::TupleStruct {
                    name: "DomainPart",
                    len: 1,
                },
                serde_test::Token::BorrowedStr("[::1]"),
                serde_test::Token::TupleStructEnd,
            ],
        );

        serde_test::assert_de_tokens(
            &DomainPart(String::from("[::1]")),
            &[
                serde_test::Token::TupleStruct {
                    name: "DomainPart",
                    len: 1,
                },
                serde_test::Token::String("[::1]"),
                serde_test::Token::TupleStructEnd,
            ],
        );

        serde_test::assert_de_tokens(
            &DomainPart(String::from("domain.example")),
            &[
                serde_test::Token::TupleStruct {
                    name: "DomainPart",
                    len: 1,
                },
                serde_test::Token::BorrowedStr("domain.example"),
                serde_test::Token::TupleStructEnd,
            ],
        );

        serde_test::assert_de_tokens_error::<DomainPart>(
            &[
                serde_test::Token::TupleStruct {
                    name: "DomainPart",
                    len: 1,
                },
                serde_test::Token::BorrowedStr("invalid@domain"),
                serde_test::Token::TupleStructEnd,
            ],
            "domain doesn’t pass idna validation",
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn resourcepart_serde() {
        serde_test::assert_de_tokens(
            &ResourcePart(String::from("test")),
            &[
                serde_test::Token::TupleStruct {
                    name: "ResourcePart",
                    len: 1,
                },
                serde_test::Token::BorrowedStr("test"),
                serde_test::Token::TupleStructEnd,
            ],
        );

        serde_test::assert_de_tokens(
            &ResourcePart(String::from("test")),
            &[
                serde_test::Token::TupleStruct {
                    name: "ResourcePart",
                    len: 1,
                },
                serde_test::Token::String("test"),
                serde_test::Token::TupleStructEnd,
            ],
        );

        serde_test::assert_de_tokens_error::<ResourcePart>(
            &[
                serde_test::Token::TupleStruct {
                    name: "ResourcePart",
                    len: 1,
                },
                serde_test::Token::BorrowedStr("🤖"),
                serde_test::Token::TupleStructEnd,
            ],
            "resource doesn’t pass resourceprep validation",
        );
    }

    #[test]
    fn unescape() {
        let node = NodePart::new("foo\\40bar").unwrap();
        assert_eq!(node.unescape().unwrap(), "foo@bar");

        let node = NodePart::new("\\22\\26\\27\\2f\\20\\3a\\3c\\3e\\40\\5c").unwrap();
        assert_eq!(node.unescape().unwrap(), "\"&'/ :<>@\\");

        let node = NodePart::new("\\20foo").unwrap();
        node.unescape().unwrap_err();

        let node = NodePart::new("foo\\20").unwrap();
        node.unescape().unwrap_err();

        let jid = BareJid::new("tréville\\40musketeers.lit@smtp.gascon.fr").unwrap();
        let node = jid.node().unwrap();
        assert_eq!(node.unescape().unwrap(), "tréville@musketeers.lit");

        // Those come from section 5.1 from the XEP.
        let data = [
            ("space cadet@example.com", "space\\20cadet@example.com"),
            (
                "call me \"ishmael\"@example.com",
                "call\\20me\\20\\22ishmael\\22@example.com",
            ),
            ("at&t guy@example.com", "at\\26t\\20guy@example.com"),
            ("d'artagnan@example.com", "d\\27artagnan@example.com"),
            ("/.fanboy@example.com", "\\2f.fanboy@example.com"),
            ("::foo::@example.com", "\\3a\\3afoo\\3a\\3a@example.com"),
            ("<foo>@example.com", "\\3cfoo\\3e@example.com"),
            ("user@host@example.com", "user\\40host@example.com"),
            ("c:\\net@example.com", "c\\3a\\net@example.com"),
            ("c:\\\\net@example.com", "c\\3a\\\\net@example.com"),
            (
                "c:\\cool stuff@example.com",
                "c\\3a\\cool\\20stuff@example.com",
            ),
            ("c:\\5commas@example.com", "c\\3a\\5c5commas@example.com"),
        ];
        for (unescaped, escaped) in data {
            let jid = BareJid::new(escaped).unwrap();
            let node = jid.node().unwrap();
            assert_eq!(
                alloc::format!("{}@example.com", node.unescape().unwrap()),
                unescaped
            );
        }
    }
}
