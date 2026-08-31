extern crate pom;

use pom::char_class::{alpha, hex_digit, oct_digit, multispace};
use pom::Parser;
use pom::parser::*;
use pom::DataInput;
use std::collections::HashMap;

use std::str::FromStr;

#[derive(Debug)]
pub enum Value {
    LiteralString(Vec<u8>),
    Name(Vec<u8>),
    Number(String),
    Integer(i64),
    Array(Vec<Value>),
    Procedure(Vec<Value>),
    Operator(String),
    Boolean(bool),
    Dictionary(HashMap<String, Value>),
}

fn hex_char() -> Parser<u8, u8> {
    let number = is_a(hex_digit).repeat(2);
    number.collect().convert(|v|u8::from_str_radix(&String::from_utf8(v).unwrap(), 16))
}

fn comment() -> Parser<u8, ()> {
    sym(b'%') * none_of(b"\r\n").repeat(0..) * eol().discard()
}

fn content_space() -> Parser<u8, ()> {
    is_a(multispace).repeat(0..).discard()
}

fn operator() -> Parser<u8, String> {
    (is_a(alpha) | one_of(b"*'\"")).repeat(1..).convert(|v|String::from_utf8(v))
}

fn oct_char() -> Parser<u8, u8> {
    let number = is_a(oct_digit).repeat(1..4);
    number.collect().convert(|v|u8::from_str_radix(&String::from_utf8(v).unwrap(), 8))
}

fn escape_sequence() -> Parser<u8, Vec<u8>> {
    sym(b'\\') *
        ( sym(b'\\').map(|_| vec![b'\\'])
            | sym(b'(').map(|_| vec![b'('])
            | sym(b')').map(|_| vec![b')'])
            | sym(b'n').map(|_| vec![b'\n'])
            | sym(b'r').map(|_| vec![b'\r'])
            | sym(b't').map(|_| vec![b'\t'])
            | sym(b'b').map(|_| vec![b'\x08'])
            | sym(b'f').map(|_| vec![b'\x0C'])
            | oct_char().map(|c| vec![c])
            | eol()     .map(|_| vec![])
            | empty()   .map(|_| vec![])
        )
}

fn nested_literal_string() -> Parser<u8, Vec<u8>> {
    sym(b'(') *
        ( none_of(b"\\()").repeat(1..)
            | escape_sequence()
            | call(nested_literal_string)
        ).repeat(0..).map(|segments| {
            let mut bytes = segments.into_iter().fold(
                vec![b'('],
                |mut bytes, mut segment| {
                    bytes.append(&mut segment);
                    bytes
                });
            bytes.push(b')');
            bytes
        })
        - sym(b')')
}

fn literal_string() -> Parser<u8, Vec<u8>> {
    sym(b'(') *
        ( none_of(b"\\()").repeat(1..)
            | escape_sequence()
            | nested_literal_string()
        ).repeat(0..).map(|segments|segments.concat())
        - sym(b')')
}

fn name() -> Parser<u8, Vec<u8>> {
    sym(b'/') * (none_of(b" \t\n\r\x0C()<>[]{}/%#") | sym(b'#') * hex_char()).repeat(0..)
}

fn integer() -> Parser<u8, i64> {
    let number = one_of(b"+-").opt() + one_of(b"0123456789").repeat(1..);
    number.collect().convert(|v|String::from_utf8(v)).convert(|s|i64::from_str(&s))
}

fn number() -> Parser<u8, String> {
    let number = one_of(b"+-").opt() +
        ( (one_of(b"0123456789") - one_of(b"0123456789").repeat(0..).discard())
            | (one_of(b"0123456789").repeat(1..) * sym(b'.') - one_of(b"0123456789").repeat(0..))
            | sym(b'.') - one_of(b"0123456789").repeat(1..)
        );
    number.collect().convert(|v|String::from_utf8(v))
}

fn space() -> Parser<u8, ()> {
    ( one_of(b" \t\n\r\0\x0C").repeat(1..).discard()
    ).repeat(0..).discard()
}

// Dictionaries are not mentioned in the CMap spec but are produced by software like Cairo and Skia and supported other by readers
fn dictionary() -> Parser<u8, HashMap<String, Value>> {
    let entry = name() - space() + call(value);
    let entries = seq(b"<<") * space() * entry.repeat(0..) - seq(b">>");
    entries.map(|entries| entries.into_iter().fold(
        HashMap::new(),
        |mut dict: HashMap<String, Value>, (key, value)| { dict.insert(String::from_utf8(key).unwrap(), value); dict }
    ))
}

fn hexadecimal_string() -> Parser<u8, Vec<u8>> {
    sym(b'<') * hex_char().repeat(0..) - sym(b'>')
}

fn eol() -> Parser<u8, u8> {
    sym(b'\r') * sym(b'\n') | sym(b'\n') | sym(b'\r')
}

fn value() -> Parser<u8, Value> {
    ( seq(b"true").map(|_| Value::Boolean(true))
    | seq(b"false").map(|_| Value::Boolean(false))
    | integer().map(|v| Value::Integer(v))
    | number().map(|v| Value::Number(v))
    | name().map(|v| Value::Name(v))
    | operator().map(|v| Value::Operator(v))
    | literal_string().map(|v| Value::LiteralString(v))
    | dictionary().map(|v| Value::Dictionary(v))
    | hexadecimal_string().map(|v| Value::LiteralString(v))
    | array().map(|v| Value::Array(v))
    | procedure().map(|v| Value::Procedure(v))
    ) - content_space()
}



fn array() -> Parser<u8, Vec<Value>> {
    sym(b'[') * space() * call(value).repeat(0..) - sym(b']')
}

fn procedure() -> Parser<u8, Vec<Value>> {
    sym(b'{') * space() * call(value).repeat(0..) - sym(b'}')
}




fn file() -> Parser<u8,Vec<Value>>
{
    ((content_space() * comment()).repeat(0..) * content_space() * value()).repeat(1..)
}

pub fn parse(input: &[u8]) -> Result<Vec<Value>, pom::Error> {
    file().parse(&mut DataInput::new(input))
}


pub fn get_encoding_map(input: &[u8]) -> Result<HashMap<u32, Vec<u8>>, &'static str> {
    let lexed = parse(&input).expect("failed to parse");

    let mut i = 0;
    let mut map = HashMap::new();
    while i < lexed.len() {
        match lexed[i] {
            Value::Operator(ref o) => {
                match o.as_ref() {
                    "array" => {
                        let count = if let &Value::Integer(ref c) = &lexed[i-1] { Ok(*c) } else { Err("array expected int") }?;
                        let name = if let &Value::Name(ref n) = &lexed[i-2] { Ok(n) } else { Err("expected name") }?;
                        i += 1;
                        if name == b"Encoding" {
                            while i < lexed.len() {
                                match lexed[i] {
                                    Value::Operator(ref o) => {
                                        match o.as_ref() {
                                            "put" => {
                                                let name = if let &Value::Name(ref n) = &lexed[i-1] { Ok(n) } else { Err("expected name") }?;
                                                let id = if let &Value::Integer(ref c) = &lexed[i-2] { Ok(*c) } else { Err("array expected int") }?;
                                                map.insert(id as u32, name.clone());
                                            }
                                            "def" => {
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                    _ => {}
                                }
                                i += 1;
                            }
                        }
                    }
                    _ => { i += 1; }
                }

            }
            _ => { i += 1; }
        }
    }
    Ok(map)

}


#[cfg(test)]
mod tests {
    use parse;
    use std::fs::File;
    use std::io::BufReader;
    use std::io::Read;

    fn do_parse(input: &[u8]) {
        let result = parse(input).unwrap();
    }
    #[test]
    fn it_works() {
        let comments = b"%foo\n%foo\ntrue";
        do_parse(comments);

        let comments_with_space_inbetween = b"%foo\n\n%foo\ntrue";
        do_parse(comments_with_space_inbetween);

    }
}
