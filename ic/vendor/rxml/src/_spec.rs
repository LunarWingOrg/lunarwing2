#![cfg(doc)]
/*!
# Restricted XML 1.0-1

Restricted XML is based on [XML 1.0 (Fifth Edition)][XML] and [Namespaces in XML 1.0 (Third edition)][XML-NAMES]. As the name suggests, the features offered by Restricted XML are a subset of the features offered by those standards. This document details exactly which restrictions apply for Restricted XML.

All Restricted XML documents are well-formed XML documents, but not vice versa.

Unless specified otherwise, violations of restricted XML criteria produce generic syntax errors.

## Grammar changes

- If the parser is configured with [`CommentMode::Reject`][`crate::parser::CommentMode`] (the default), the `Comment` production and all references to it are deleted. Encountering the unescaped `<!--` sequence in places where the `Comment` production was previously referenced MUST produce an error which MAY be a [`Error::RestrictedXml`].
- The `PI` production and all references to it are deleted. Encountering the unescaped `<?` sequence in places where the `PI` production was previously referenced MUST produce an error which MAY be a [`Error::RestrictedXml`].
- Consequently:

  - if the parser is configured with [`CommentMode::Reject`][`crate::parser::CommentMode`], the `Misc` production becomes equivalent to `S` (whitespace).
  - otherwise, it gets reduced to to `Comment | S`.
- The `doctypedecl` production and all references to it are deleted.
- To avoid ambuguities, the `prolog` production is rewritten to:

	```text
	prolog ::= XMLDecl? Misc*
	```

	Without this explicit rewrite, the right-hand side would now be `XMLDecl? Misc* (Misc*)?`, which is nonsensical and confusing at best, and exponential at worst.

## Well-formedness changes

- The `standalone` attribute on the XML declaration MUST be `yes` or absent. Violations must produce an [`Error::RestrictedXml`] error.
- The `encoding` attribute on the XML declaration MUST be `utf-8` or absent. Violations must produce an [`Error::RestrictedXml`] error.

## Violations

- §2.2 states that:

	> All XML processors MUST accept the UTF-8 and UTF-16 encodings of Unicode \[Unicode]; the mechanisms for signaling which of the two is in use, or for bringing other encodings into play, are discussed later, in 4.3.3 Character Encoding in Entities.

	Restricted XML only supports the UTF-8 encoding.

## Notes

The elimination of the `doctypedecl` production implies the removal of all non-predefined references. That means that a Restricted XML parser MUST NOT accept any entity references except `lt`, `amp`, `gt`, `quot`, `apos`. Violations must produce an [`Error::RestrictedXml`] error. Character references (like `&#20;`) are unaffected by this.

   [XML]: https://www.w3.org/TR/REC-xml/
   [XML-NAMES]: https://www.w3.org/TR/REC-xml-names/

*/
#[allow(dead_code)]
use crate::Error;
