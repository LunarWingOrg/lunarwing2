use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use std::println;

use super::*;

use crate::error::EndOrError;

#[cfg(feature = "tokio")]
use tokio;

#[test]
fn long_element_names() {
	let doc = b"<jitsi_participant_codecType>vp9</jitsi_participant_codecType>";

	let mut fp = Parser::default();
	let mut out = Vec::<Event>::new();
	let mut doc_buf = &doc[..];
	match fp.parse_all(&mut doc_buf, false, |ev| {
		out.push(ev);
	}) {
		Err(EndOrError::NeedMoreData) => (),
		other => panic!("unexpected result: {:?}", other),
	}
}

#[test]
fn restricted_xml_for_xml_stylesheet() {
	let doc = b"<?xml version='1.0'?>\n<?xml-stylesheet?>";

	let mut fp = Parser::default();
	let mut out = Vec::<Event>::new();
	let mut doc_buf = &doc[..];
	match fp.parse_all(&mut doc_buf, false, |ev| {
		out.push(ev);
	}) {
		Err(EndOrError::Error(Error::RestrictedXml(_))) => (),
		other => panic!("unexpected result: {:?}", other),
	}
}

#[test]
fn restricted_xml_for_late_xml_stylesheets() {
	let doc = b"<?xml version='1.0'?>\n<root><?xml-stylesheet?></root>";

	let mut fp = Parser::default();
	let mut out = Vec::<Event>::new();
	let mut doc_buf = &doc[..];
	match fp.parse_all(&mut doc_buf, false, |ev| {
		out.push(ev);
	}) {
		Err(EndOrError::Error(Error::RestrictedXml(_))) => (),
		other => panic!("unexpected result: {:?}", other),
	}
}

// note that this is just a smoketest... the components of the FeedParser
// are tested extensively in the modules.
#[test]
fn feedparser_can_read_xml_document() {
	let doc = b"<?xml version='1.0'?>\n<root xmlns='urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4' a=\"foo\" b='bar'><child>with some text</child></root>";

	let mut fp = Parser::default();
	let mut out = Vec::<Event>::new();
	let mut doc_buf = &doc[..];
	match fp.parse_all(&mut doc_buf, false, |ev| {
		out.push(ev);
	}) {
		Err(EndOrError::NeedMoreData) => (),
		other => panic!("unexpected result: {:?}", other),
	};

	{
		let mut iter = out.iter();
		match iter.next().unwrap() {
			Event::XmlDeclaration(em, XmlVersion::V1_0) => {
				assert_eq!(em.len(), 21);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				// note: 77 because of the \n between xml decl and whitespace. see also comment on EventMetrics
				assert_eq!(em.len(), 77);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "root");
				assert_eq!(attrs.len(), 2);
				assert_eq!(attrs.get(Namespace::none(), "a").unwrap(), "foo");
				assert_eq!(attrs.get(Namespace::none(), "b").unwrap(), "bar");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				assert_eq!(em.len(), 7);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "child");
				assert_eq!(attrs.len(), 0);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::Text(em, cdata) => {
				assert_eq!(em.len(), 14);
				assert_eq!(cdata, "with some text");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 8);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		};
	}

	match fp.parse_all(&mut doc_buf, true, |ev| {
		panic!("unexpected event: {:?}", ev)
	}) {
		Ok(()) => (),
		other => panic!("unexpected result: {:?}", other),
	};
}

#[test]
fn feedparser_can_handle_chunked_input() {
	let doc = "<?xml version='1.0'?><root xmlns='urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4' a=\"foo\" b='bar'><child>with some text🐱😸😹😺😻😼😾😿🙀</child></root>".as_bytes();

	for n in 1..doc.len() {
		let mut fp = Parser::default();
		let mut out = Vec::<Event>::new();
		for mut chunk in doc.chunks(n) {
			loop {
				match fp.parse(&mut chunk, false) {
					Err(EndOrError::NeedMoreData) => break,
					Err(EndOrError::Error(other)) => panic!("unexpected error: {:?}", other),
					Ok(Some(ev)) => out.push(ev),
					Ok(None) => break,
				}
			}
			assert_eq!(chunk.len(), 0);
		}

		{
			let mut iter = out.iter();
			match iter.next().unwrap() {
				Event::XmlDeclaration(em, XmlVersion::V1_0) => {
					assert_eq!(em.len(), 21);
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::StartElement(em, (nsuri, localname), attrs) => {
					assert_eq!(em.len(), 76);
					assert_eq!(
						nsuri.as_str(),
						"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
					);
					assert_eq!(localname, "root");
					assert_eq!(attrs.len(), 2);
					assert_eq!(attrs.get(Namespace::none(), "a").unwrap(), "foo");
					assert_eq!(attrs.get(Namespace::none(), "b").unwrap(), "bar");
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::StartElement(em, (nsuri, localname), attrs) => {
					assert_eq!(em.len(), 7);
					assert_eq!(
						nsuri.as_str(),
						"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
					);
					assert_eq!(localname, "child");
					assert_eq!(attrs.len(), 0);
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::Text(em, cdata) => {
					assert_eq!(em.len(), 50);
					assert_eq!(cdata, "with some text🐱😸😹😺😻😼😾😿🙀");
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::EndElement(em) => {
					assert_eq!(em.len(), 8);
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::EndElement(em) => {
					assert_eq!(em.len(), 7);
				}
				other => panic!("unexpected event: {:?}", other),
			};
		}

		match fp.parse_all(&mut &[][..], true, |ev| {
			panic!("unexpected event: {:?}", ev)
		}) {
			Ok(()) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}
}

#[test]
fn feedparser_can_handle_buf_with_multiple_chunks() {
	let doc = "<?xml version='1.0'?><root xmlns='urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4' a=\"foo\" b='bar'><child>with some text🐱😸😹😺😻😼😾😿🙀</child></root>".as_bytes();

	for n in 1..doc.len() {
		let mut fp = Parser::default();
		let mut out = Vec::<Event>::new();
		let mut buf = VecDeque::with_capacity(doc.len());
		for i in n..doc.len() {
			buf.push_back(doc[i]);
		}
		for i in (0..n).rev() {
			buf.push_front(doc[i]);
		}
		assert_eq!(buf.as_slices().0, &doc[..n]);
		assert_eq!(buf.as_slices().1, &doc[n..]);
		match fp.parse_all_buf(&mut buf, true, |ev| {
			out.push(ev);
		}) {
			Ok(()) => (),
			other => panic!("unexpected parse_all_buf result: {:?}", other),
		}

		{
			let mut iter = out.iter();
			match iter.next().unwrap() {
				Event::XmlDeclaration(em, XmlVersion::V1_0) => {
					assert_eq!(em.len(), 21);
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::StartElement(em, (nsuri, localname), attrs) => {
					assert_eq!(em.len(), 76);
					assert_eq!(
						nsuri.as_str(),
						"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
					);
					assert_eq!(localname, "root");
					assert_eq!(attrs.len(), 2);
					assert_eq!(attrs.get(Namespace::none(), "a").unwrap(), "foo");
					assert_eq!(attrs.get(Namespace::none(), "b").unwrap(), "bar");
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::StartElement(em, (nsuri, localname), attrs) => {
					assert_eq!(em.len(), 7);
					assert_eq!(
						nsuri.as_str(),
						"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
					);
					assert_eq!(localname, "child");
					assert_eq!(attrs.len(), 0);
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::Text(em, cdata) => {
					assert_eq!(em.len(), 50);
					assert_eq!(cdata, "with some text🐱😸😹😺😻😼😾😿🙀");
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::EndElement(em) => {
					assert_eq!(em.len(), 8);
				}
				other => panic!("unexpected event: {:?}", other),
			};
			match iter.next().unwrap() {
				Event::EndElement(em) => {
					assert_eq!(em.len(), 7);
				}
				other => panic!("unexpected event: {:?}", other),
			};
		}

		match fp.parse_all(&mut &[][..], true, |ev| {
			panic!("unexpected event: {:?}", ev)
		}) {
			Ok(()) => (),
			other => panic!("unexpected result: {:?}", other),
		};
	}
}

// note that this is just a smoketest... the components of the PullParser
// are tested extensively in the modules.
#[test]
fn pullparser_can_read_xml_document() {
	let mut doc = &b"<?xml version='1.0'?>\n<root xmlns='urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4' a=\"foo\" b='bar'><child>with some text</child></root>\n"[..];

	let mut pp = Reader::<_>::new(&mut doc);
	let mut out = Vec::<Event>::new();
	let result = pp.read_all(|ev| {
		out.push(ev);
	});
	assert_eq!(result.unwrap(), ());

	{
		let mut iter = out.iter();
		match iter.next().unwrap() {
			Event::XmlDeclaration(em, XmlVersion::V1_0) => {
				assert_eq!(em.len(), 21);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				// note: 77 because of the \n between xml decl and whitespace. see also comment on EventMetrics
				assert_eq!(em.len(), 77);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "root");
				assert_eq!(attrs.len(), 2);
				assert_eq!(attrs.get(Namespace::none(), "a").unwrap(), "foo");
				assert_eq!(attrs.get(Namespace::none(), "b").unwrap(), "bar");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				assert_eq!(em.len(), 7);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "child");
				assert_eq!(attrs.len(), 0);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::Text(em, cdata) => {
				assert_eq!(em.len(), 14);
				assert_eq!(cdata, "with some text");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 8);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		};
	}
}

/// This is only used to drop-in tests with util/fuzz-to-test.py
#[allow(dead_code)]
fn run_fuzz_test(mut data: &[u8]) -> Result<()> {
	let mut fp = Parser::default();
	loop {
		match fp.parse(&mut data, true) {
			Ok(None) => return Ok(()),
			// we passed at_eof = true, i.e. this should always cause a hard
			// error.
			Err(EndOrError::NeedMoreData) => unreachable!(),
			Err(EndOrError::Error(e)) => return Err(e),
			Ok(Some(_)) => (),
		}
	}
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn asyncparser_can_read_xml_document() {
	let doc = b"<?xml version='1.0'?>\n<root xmlns='urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4' a=\"foo\" b='bar'><child>with some text</child></root>";

	let mut r = &doc[..];
	let mut ap = AsyncReader::<_>::new(&mut r);
	let mut out = Vec::<Event>::new();
	let result = ap
		.read_all(|ev| {
			out.push(ev);
		})
		.await;
	result.unwrap();

	{
		let mut iter = out.iter();
		match iter.next().unwrap() {
			Event::XmlDeclaration(em, XmlVersion::V1_0) => {
				assert_eq!(em.len(), 21);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				// note: 77 because of the \n between xml decl and whitespace. see also comment on EventMetrics
				assert_eq!(em.len(), 77);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "root");
				assert_eq!(attrs.len(), 2);
				assert_eq!(attrs.get(Namespace::none(), "a").unwrap(), "foo");
				assert_eq!(attrs.get(Namespace::none(), "b").unwrap(), "bar");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				assert_eq!(em.len(), 7);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "child");
				assert_eq!(attrs.len(), 0);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::Text(em, cdata) => {
				assert_eq!(em.len(), 14);
				assert_eq!(cdata, "with some text");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 8);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		};
	}
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn asyncparser_can_handle_chunked_input() {
	let doc = "<?xml version='1.0'?>\n<root xmlns='urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4' a=\"foo\" b='bar'><child>with some text🐱😸😹😺😻😼😾😿🙀</child></root>".as_bytes();

	let mut r = &doc[..];
	let mut r = tokio::io::BufReader::with_capacity(4, &mut r);
	let mut ap = AsyncReader::<_>::new(&mut r);
	let mut out = Vec::<Event>::new();
	let result = ap
		.read_all(|ev| {
			out.push(ev);
		})
		.await;
	result.unwrap();

	{
		let mut iter = out.iter();
		match iter.next().unwrap() {
			Event::XmlDeclaration(em, XmlVersion::V1_0) => {
				assert_eq!(em.len(), 21);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				// note: 77 because of the \n between xml decl and whitespace. see also comment on EventMetrics
				assert_eq!(em.len(), 77);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "root");
				assert_eq!(attrs.len(), 2);
				assert_eq!(attrs.get(Namespace::none(), "a").unwrap(), "foo");
				assert_eq!(attrs.get(Namespace::none(), "b").unwrap(), "bar");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::StartElement(em, (nsuri, localname), attrs) => {
				assert_eq!(em.len(), 7);
				assert_eq!(
					nsuri.as_str(),
					"urn:uuid:fab98e86-7c09-477c-889c-0313d9877bb4"
				);
				assert_eq!(localname, "child");
				assert_eq!(attrs.len(), 0);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::Text(em, cdata) => {
				assert_eq!(em.len(), 50);
				assert_eq!(cdata, "with some text🐱😸😹😺😻😼😾😿🙀");
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 8);
			}
			other => panic!("unexpected event: {:?}", other),
		};
		match iter.next().unwrap() {
			Event::EndElement(em) => {
				assert_eq!(em.len(), 7);
			}
			other => panic!("unexpected event: {:?}", other),
		};
	}
}

#[test]
fn codeberg_issue_15() {
	use crate::{Parse as _, Parser};
	let mut document = String::with_capacity(6 + 7 + 10000 * 3);
	document.push_str("<text>");
	for _ in 0..2731 {
		// Try increasing it if no error has occurred
		document.push_str("あ"); // U+3042, UTF-8: e3 81 82
	}
	document.push_str("</text>");

	let mut document = document.as_bytes();
	let mut parser = Parser::new();

	while !document.is_empty() {
		let event = parser.parse(&mut document, true).unwrap(); // panic: InvalidUtf8Byte(0xe3)
		println!("{event:?}")
	}
}
