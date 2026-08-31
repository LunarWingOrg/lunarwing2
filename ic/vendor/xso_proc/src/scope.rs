// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Identifiers used within generated code.

use proc_macro2::Span;
use syn::*;

use crate::types::ref_ty;

/// Container struct for various identifiers used throughout the parser code.
///
/// This struct is passed around from the [`crate::compound::Compound`]
/// downward to the code generators in order to ensure that everyone is on the
/// same page about which identifiers are used for what.
///
/// The recommended usage is to bind the names which are needed into the local
/// scope like this:
///
/// ```text
/// # let scope = FromEventsScope::new();
/// let FromEventsScope {
///     ref attrs,
///     ..
/// } = scope;
/// ```
pub(crate) struct FromEventsScope {
    /// Accesses the `AttrMap` from code in
    /// [`crate::field::FieldBuilderPart::Init`].
    pub(crate) attrs: Ident,

    /// Accesses the `String` of a `rxml::Event::Text` event from code in
    /// [`crate::field::FieldBuilderPart::Text`].
    pub(crate) text: Ident,

    /// Accesses the builder data during parsing.
    ///
    /// This should not be used directly outside [`crate::compound`]. Most of
    /// the time, using [`Self::access_field`] is the correct way to access
    /// the builder data.
    pub(crate) builder_data_ident: Ident,

    /// Accesses the result produced by a nested state's builder type.
    ///
    /// See [`crate::field::FieldBuilderPart::Nested`].
    pub(crate) substate_data: Ident,

    /// Accesses the result produced by a nested state's builder type.
    ///
    /// See [`crate::field::FieldBuilderPart::Nested`].
    pub(crate) substate_result: Ident,

    /// Prefix which should be used for any types which are declared, to
    /// ensure they don't collide with other names.
    pub(crate) type_prefix: Ident,
}

impl FromEventsScope {
    /// Create a fresh scope with all necessary identifiers.
    pub(crate) fn new(type_prefix: Ident) -> Self {
        // Sadly, `Ident::new` is not `const`, so we have to create even the
        // well-known identifiers from scratch all the time.
        Self {
            attrs: Ident::new("attrs", Span::call_site()),
            text: Ident::new("__xso_proc_macro_text_data", Span::call_site()),
            builder_data_ident: Ident::new("__xso_proc_macro_builder_data", Span::call_site()),
            substate_data: Ident::new("__xso_proc_macro_substate_data", Span::call_site()),
            substate_result: Ident::new("__xso_proc_macro_substate_result", Span::call_site()),
            type_prefix,
        }
    }

    /// Generate an expression which accesses the temporary value for the
    /// given `member` during parsing.
    pub(crate) fn access_field(&self, member: &Member) -> Expr {
        Expr::Field(ExprField {
            attrs: Vec::new(),
            base: Box::new(Expr::Path(ExprPath {
                attrs: Vec::new(),
                qself: None,
                path: self.builder_data_ident.clone().into(),
            })),
            dot_token: syn::token::Dot {
                spans: [Span::call_site()],
            },
            member: Member::Named(mangle_member(member)),
        })
    }

    /// Generate an ident with proper scope and span from the type prefix and
    /// the given member and actual type name.
    ///
    /// Due to being merged with the type prefix of this scope and the given
    /// member, this type name is guaranteed to be unique for unique values of
    /// `name`.
    pub(crate) fn make_member_type_name(&self, member: &Member, name: &str) -> Ident {
        quote::format_ident!(
            "{}Member{}{}",
            self.type_prefix,
            match member {
                Member::Named(ref ident) => ident.to_string(),
                Member::Unnamed(Index { index, .. }) => index.to_string(),
            },
            name,
        )
    }
}

/// Container struct for various identifiers used throughout the generator
/// code.
///
/// This struct is passed around from the [`crate::compound::Compound`]
/// downward to the code generators in order to ensure that everyone is on the
/// same page about which identifiers are used for what.
///
/// See [`FromEventsScope`] for recommendations on the usage.
pub(crate) struct AsItemsScope {
    /// Lifetime for data borrowed by the implementation.
    pub(crate) lifetime: Lifetime,

    /// Prefix which should be used for any types which are declared, to
    /// ensure they don't collide with other names.
    pub(crate) type_prefix: Ident,
}

impl AsItemsScope {
    /// Create a fresh scope with all necessary identifiers.
    pub(crate) fn new(lifetime: &Lifetime, type_prefix: Ident) -> Self {
        Self {
            lifetime: lifetime.clone(),
            type_prefix,
        }
    }

    /// Create a reference to `ty`, borrowed for the lifetime of the AsXml
    /// impl.
    pub(crate) fn borrow(&self, ty: Type) -> Type {
        ref_ty(ty, self.lifetime.clone())
    }

    /// Generate an ident with proper scope and span from the type prefix and
    /// the given member and actual type name.
    ///
    /// Due to being merged with the type prefix of this scope and the given
    /// member, this type name is guaranteed to be unique for unique values of
    /// `name`.
    pub(crate) fn make_member_type_name(&self, member: &Member, name: &str) -> Ident {
        quote::format_ident!(
            "{}Member{}{}",
            self.type_prefix,
            match member {
                Member::Named(ref ident) => ident.to_string(),
                Member::Unnamed(Index { index, .. }) => index.to_string(),
            },
            name,
        )
    }
}

pub(crate) fn mangle_member(member: &Member) -> Ident {
    match member {
        Member::Named(member) => quote::format_ident!("f{}", member),
        Member::Unnamed(member) => quote::format_ident!("f_u{}", member.index),
    }
}
