use criterion::{black_box, criterion_group, criterion_main, Criterion};

use rxml_validation::validate_cdata;

use bytes::{BufMut, BytesMut};

static RANDOM_CDATA: &'static str = include_str!("random-cdata.utf8");
static RANDOM_LATIN1: &'static str = include_str!("random-latin1.utf8");
static SHORT_LATIN1: &'static str = include_str!("short-latin1.utf8");

const TEXT_SPECIALS: &'static [u8] = &[b'<', b'>', b'&', b'\r'];
const ATTR_SPECIALS: &'static [u8] = &[b'"', b'\'', b'\r', b'\n', b'\t', b'<', b'>', b'&'];

static GROUPS: [(&'static str, &'static str); 3] = [
	("random unicode", RANDOM_CDATA),
	("random latin1", RANDOM_LATIN1),
	("short latin1", SHORT_LATIN1),
];

fn escape_naive<'a, B: BufMut>(out: &'a mut B, data: &'a [u8]) {
	for i in 0..data.len() {
		let ch = data[i];
		match ch {
			b'"' => out.put_slice(b"&#34;"),
			b'\'' => out.put_slice(b"&#39;"),
			b'<' => out.put_slice(b"&lt;"),
			b'>' => out.put_slice(b"&gt;"),
			b'&' => out.put_slice(b"&amp;"),
			b'\r' => out.put_slice(b"&#xd;"),
			b'\n' => out.put_slice(b"&#xa;"),
			b'\t' => out.put_slice(b"&#x9;"),
			ch => out.put_u8(ch),
		}
	}
}

fn escape_preselected<'a, B: BufMut>(out: &'a mut B, data: &'a [u8], specials: &'static [u8]) {
	let mut last_index = 0;
	for i in 0..data.len() {
		let ch = data[i];
		if !specials.contains(&ch) {
			continue;
		}
		if i > last_index {
			out.put_slice(&data[last_index..i]);
		}
		match ch {
			b'"' => out.put_slice(b"&#34;"),
			b'\'' => out.put_slice(b"&#39;"),
			b'<' => out.put_slice(b"&lt;"),
			b'>' => out.put_slice(b"&gt;"),
			b'&' => out.put_slice(b"&amp;"),
			b'\r' => out.put_slice(b"&#xd;"),
			b'\n' => out.put_slice(b"&#xa;"),
			b'\t' => out.put_slice(b"&#x9;"),
			_ => panic!("unexpected special character?!"),
		}
		last_index = i + 1;
	}
	out.put_slice(&data[last_index..data.len()]);
}

fn escape_preselected_v2<'a, B: BufMut>(out: &'a mut B, data: &'a [u8]) {
	let mut last_index = 0;
	for i in 0..data.len() {
		let ch = data[i];
		let escaped: &[u8] = match ch {
			b'"' => &b"&#34;"[..],
			b'\'' => &b"&#39;"[..],
			b'<' => &b"&lt;"[..],
			b'>' => &b"&gt;"[..],
			b'&' => &b"&amp;"[..],
			b'\r' => &b"&#xd;"[..],
			b'\n' => &b"&#xa;"[..],
			b'\t' => &b"&#x9;"[..],
			_ => continue,
		};
		if i > last_index {
			out.put_slice(&data[last_index..i]);
		}
		out.put_slice(escaped);
		last_index = i + 1;
	}
	out.put_slice(&data[last_index..data.len()]);
}

fn escape_preselected_v3<'a, B: BufMut>(out: &'a mut B, data: &'a [u8], attribute_context: bool) {
	let mut last_index = 0;
	for i in 0..data.len() {
		let ch = data[i];
		let escaped: &[u8] = match ch {
			b'"' if attribute_context => &b"&#34;"[..],
			b'\'' if attribute_context => &b"&#39;"[..],
			b'<' => &b"&lt;"[..],
			b'>' => &b"&gt;"[..],
			b'&' => &b"&amp;"[..],
			b'\r' => &b"&#xd;"[..],
			b'\n' if attribute_context => &b"&#xa;"[..],
			b'\t' if attribute_context => &b"&#x9;"[..],
			_ => continue,
		};
		if i > last_index {
			out.put_slice(&data[last_index..i]);
		}
		out.put_slice(escaped);
		last_index = i + 1;
	}
	out.put_slice(&data[last_index..data.len()]);
}

fn escape_validated<'a, B: BufMut>(
	out: &'a mut B,
	data: &'a [u8],
) -> Result<(), rxml_validation::Error> {
	let mut last_index = 0;
	for i in 0..data.len() {
		let ch = data[i];
		let escaped: &[u8] = match ch {
			b'\x00'..=b'\x08' | b'\x0b' | b'\x0c' | b'\x0e'..=b'\x1f' => {
				return Err(rxml_validation::Error::InvalidChar(ch.into()));
			}
			b'"' => &b"&#34;"[..],
			b'\'' => &b"&#39;"[..],
			b'<' => &b"&lt;"[..],
			b'>' => &b"&gt;"[..],
			b'&' => &b"&amp;"[..],
			b'\r' => &b"&#xd;"[..],
			b'\n' => &b"&#xa;"[..],
			b'\t' => &b"&#x9;"[..],
			b'\xbe' | b'\xbf' => {
				if i >= 2 && data[i - 2] == b'\xef' && data[i - 1] == b'\xbf' {
					// U+FFFE or U+FFFF
					let low_bit = ch & 0x01;
					// SAFETY: we are passing only 16 bits and the upper
					// nibble is set to all ones, so this is within the bounds
					// of a unicode code point and not a surrogate.
					return Err(rxml_validation::Error::InvalidChar(unsafe {
						char::from_u32_unchecked(0xfffe | low_bit as u32)
					}));
				} else {
					continue;
				}
			}
			_ => continue,
		};
		if i > last_index {
			out.put_slice(&data[last_index..i]);
		}
		out.put_slice(escaped);
		last_index = i + 1;
	}
	out.put_slice(&data[last_index..data.len()]);
	Ok(())
}

fn escape_validated_v2<'a, B: BufMut>(
	out: &'a mut B,
	data: &'a [u8],
	attribute_context: bool,
) -> Result<(), rxml_validation::Error> {
	let mut last_index = 0;
	for i in 0..data.len() {
		let ch = data[i];
		let escaped: &[u8] = match ch {
			b'\x00'..=b'\x08' | b'\x0b' | b'\x0c' | b'\x0e'..=b'\x1f' => {
				return Err(rxml_validation::Error::InvalidChar(ch.into()));
			}
			b'"' if attribute_context => &b"&#34;"[..],
			b'\'' if attribute_context => &b"&#39;"[..],
			b'<' => &b"&lt;"[..],
			b'>' => &b"&gt;"[..],
			b'&' => &b"&amp;"[..],
			b'\r' => &b"&#xd;"[..],
			b'\n' if attribute_context => &b"&#xa;"[..],
			b'\t' if attribute_context => &b"&#x9;"[..],
			b'\xbe' | b'\xbf' => {
				if i >= 2 && data[i - 2] == b'\xef' && data[i - 1] == b'\xbf' {
					// U+FFFE or U+FFFF
					let low_bit = ch & 0x01;
					// SAFETY: we are passing only 16 bits and the upper
					// nibble is set to all ones, so this is within the bounds
					// of a unicode code point and not a surrogate.
					return Err(rxml_validation::Error::InvalidChar(unsafe {
						char::from_u32_unchecked(0xfffe | low_bit as u32)
					}));
				} else {
					continue;
				}
			}
			_ => continue,
		};
		if i > last_index {
			out.put_slice(&data[last_index..i]);
		}
		out.put_slice(escaped);
		last_index = i + 1;
	}
	out.put_slice(&data[last_index..data.len()]);
	Ok(())
}

fn escape_validated_v3<'a, B: BufMut>(
	out: &'a mut B,
	data: &'a str,
	attribute_context: bool,
) -> Result<(), rxml_validation::Error> {
	let data = data.as_bytes();
	let mut last_index = 0;
	for i in 0..data.len() {
		let ch = data[i];
		let escaped: &[u8] = match ch {
			b'\x00'..=b'\x08' | b'\x0b' | b'\x0c' | b'\x0e'..=b'\x1f' => {
				return Err(rxml_validation::Error::InvalidChar(ch.into()));
			}
			b'"' if attribute_context => &b"&#34;"[..],
			b'\'' if attribute_context => &b"&#39;"[..],
			b'<' => &b"&lt;"[..],
			b'>' => &b"&gt;"[..],
			b'&' => &b"&amp;"[..],
			b'\r' => &b"&#xd;"[..],
			b'\n' if attribute_context => &b"&#xa;"[..],
			b'\t' if attribute_context => &b"&#x9;"[..],
			b'\xef' => {
				let (n1, n2) = unsafe {
					// SAFETY: `data` is a &str, which is UTF-8, which means
					// that 0xef must be followed by at least two more bytes.
					(*data.get_unchecked(i + 1), *data.get_unchecked(i + 2))
				};
				if n1 == b'\xbf' && (n2 == b'\xbe' || n2 == b'\xbf') {
					// U+FFFE or U+FFFF
					let low_bit = n2 & 0x01;
					// SAFETY: we are passing only 16 bits and the upper
					// nibble is set to all ones, so this is within the bounds
					// of a unicode code point and not a surrogate.
					return Err(rxml_validation::Error::InvalidChar(unsafe {
						char::from_u32_unchecked(0xfffe | low_bit as u32)
					}));
				} else {
					continue;
				}
			}
			_ => continue,
		};
		if i > last_index {
			out.put_slice(&data[last_index..i]);
		}
		out.put_slice(escaped);
		last_index = i + 1;
	}
	out.put_slice(&data[last_index..data.len()]);
	Ok(())
}

fn escape_perf(c: &mut Criterion) {
	for (group_name, group_data) in GROUPS {
		let mut group = c.benchmark_group(format!("escaping only performance: {}", group_name));

		group.bench_function("escape_naive", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				escape_naive(&mut buf, black_box(group_data.as_bytes()));
				buf.len()
			});
		});

		group.bench_function("escape_preselected_text", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				escape_preselected(&mut buf, black_box(group_data.as_bytes()), TEXT_SPECIALS);
				buf.len()
			});
		});

		group.bench_function("escape_preselected_attr", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				escape_preselected(&mut buf, black_box(group_data.as_bytes()), ATTR_SPECIALS);
				buf.len()
			});
		});

		group.bench_function("escape_preselected_v2", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				escape_preselected_v2(&mut buf, black_box(group_data.as_bytes()));
				buf.len()
			});
		});

		group.bench_function("escape_preselected_v3_text", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				escape_preselected_v3(&mut buf, black_box(group_data.as_bytes()), false);
				buf.len()
			});
		});

		group.bench_function("escape_preselected_v3_attr", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				escape_preselected_v3(&mut buf, black_box(group_data.as_bytes()), true);
				buf.len()
			});
		});
	}
}

fn validate_perf(c: &mut Criterion) {
	for (group_name, group_data) in GROUPS {
		let mut group =
			c.benchmark_group(format!("escaping+validation performance: {}", group_name));

		group.bench_function("validate_escape_preselected_text", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				let group_data = black_box(group_data);
				assert!(validate_cdata(group_data).is_ok());
				escape_preselected(&mut buf, group_data.as_bytes(), TEXT_SPECIALS);
				buf.len()
			});
		});

		group.bench_function("validate_escape_preselected_attr", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				let group_data = black_box(group_data);
				assert!(validate_cdata(group_data).is_ok());
				escape_preselected(&mut buf, group_data.as_bytes(), ATTR_SPECIALS);
				buf.len()
			});
		});

		group.bench_function("validate_escape_preselected_v2", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				let group_data = black_box(group_data);
				assert!(validate_cdata(group_data).is_ok());
				escape_preselected_v2(&mut buf, group_data.as_bytes());
				buf.len()
			});
		});

		group.bench_function("validate_escape_preselected_v3_text", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				let group_data = black_box(group_data);
				assert!(validate_cdata(group_data).is_ok());
				escape_preselected_v3(&mut buf, group_data.as_bytes(), false);
				buf.len()
			});
		});

		group.bench_function("validate_escape_preselected_v3_attr", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				let group_data = black_box(group_data);
				assert!(validate_cdata(group_data).is_ok());
				escape_preselected_v3(&mut buf, group_data.as_bytes(), true);
				buf.len()
			});
		});

		group.bench_function("escape_validated", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				assert!(escape_validated(&mut buf, black_box(group_data.as_bytes())).is_ok());
				buf.len()
			});
		});

		group.bench_function("escape_validated_v2_text", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				assert!(
					escape_validated_v2(&mut buf, black_box(group_data.as_bytes()), false).is_ok()
				);
				buf.len()
			});
		});

		group.bench_function("escape_validated_v2_attr", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				assert!(
					escape_validated_v2(&mut buf, black_box(group_data.as_bytes()), true).is_ok()
				);
				buf.len()
			});
		});

		group.bench_function("escape_validated_v3_text", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				assert!(escape_validated_v3(&mut buf, black_box(group_data), false).is_ok());
				buf.len()
			});
		});

		group.bench_function("escape_validated_v3_attr", |b| {
			let mut buf = BytesMut::new();
			buf.reserve((group_data.len() * 15) / 10);
			b.iter(|| {
				buf.clear();
				assert!(escape_validated_v3(&mut buf, black_box(group_data), true).is_ok());
				buf.len()
			});
		});
	}
}

criterion_group! {
	name = benches;
	config = Criterion::default().sample_size(300);
	targets = escape_perf, validate_perf
}
criterion_main!(benches);
