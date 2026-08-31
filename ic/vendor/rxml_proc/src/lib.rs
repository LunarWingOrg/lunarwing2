/*!
# Macros for XML strings

This crate provides macros to check XML string syntax at compile time.

## Example

```rust,ignore
use rxml::{NcNameStr, xml_cdata, xml_ncname};

const XML_PREFIX: &'static NcNameStr = xml_ncname!("xml");
const XML_QNAME: &'static NameStr = xml_name!("xml:lang");
```

## See also

This crate bases on the [`rxml_validation`] crate and it primarily intended
for use with the [`rxml`](https://docs.rs/rxml) crate.
*/
use proc_macro::TokenStream;
use quote::quote;
use rxml_validation::{validate_name, validate_ncname};
use syn::{
	parse::{Parse, ParseStream},
	*,
};

struct Input {
	data: LitStr,
	ty_mod: Path,
}

impl Parse for Input {
	fn parse(input: ParseStream) -> Result<Self> {
		let data = input.parse()?;
		input.parse::<Token![,]>()?;
		let ty_mod = input.parse()?;
		if input.peek(Token![,]) {
			// consume a trailing comma
			input.parse::<Token![,]>()?;
		}
		Ok(Self { data, ty_mod })
	}
}

/** XML 1.0 Name compliant string

# Example

```rust,ignore
use rxml::{NameStr, xml_name};

const FORBIDDEN: &'static NameStr = xml_name!("xmlns:xml");
```

# Safety

The raw version of this macro, as exported by the `rxml_proc` crate (*not* by
the `rxml` crate!), expects a second argument which must be a path pointing
at the `rxml` crate.

If another path is passed, the behaviour of the generated is literally
undefined, as the generated code then contains a transmute with the wrong type
argument.
*/
#[proc_macro]
pub fn xml_name(input: TokenStream) -> TokenStream {
	let Input { data, ty_mod } = parse_macro_input!(input);
	let s = data.value();
	let tokens = match validate_name(&s) {
		Ok(()) => quote! { core::mem::transmute::<_, &#ty_mod::NameStr>(#s) },
		Err(e) => {
			let err = format!("invalid Name string {:?}: {}", s, e);
			quote! { compile_error!(#err) }
		}
	};
	tokens.into()
}

/** Namespaces for XML 1.0 NCName compliant string

# Example

```rust,ignore
use rxml::{NcNameStr, xml_ncname};

const XML_PREFIX: &'static NCNameStr = xml_ncname!("xml");
```

# Safety

The raw version of this macro, as exported by the `rxml_proc` crate (*not* by
the `rxml` crate!), expects a second argument which must be a path pointing
at the `rxml` crate.

If another path is passed, the behaviour of the generated is literally
undefined, as the generated code then contains a transmute with the wrong type
argument.
*/
#[proc_macro]
pub fn xml_ncname(input: TokenStream) -> TokenStream {
	let Input { data, ty_mod } = parse_macro_input!(input);
	let s = data.value();
	let tokens = match validate_ncname(&s) {
		Ok(()) => quote! { core::mem::transmute::<_, &#ty_mod::NcNameStr>(#s) },
		Err(e) => {
			let err = format!("invalid NCName string {:?}: {}", s, e);
			quote! { compile_error!(#err) }
		}
	};
	tokens.into()
}
