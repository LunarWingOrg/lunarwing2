xso -- serde-like parsing for XML
=================================

What’s this?
------------

This crate provides the traits for parsing XML data into Rust structs, and
vice versa. You can do things like:

```rust
#[derive(FromXml, AsXml)]
#[xml(namespace = "urn:example", name = "element")]
pub struct Foo;
```

For more information, see
[its documentation on docs.rs](https://docs.rs/xso/latest/xso/) for the latest
release or
[the documentation for the main branch on our servers](https://docs.xmpp.rs/main/xso/).

What license is it under?
-------------------------

MPL-2.0 or later, see the `LICENSE` file.
