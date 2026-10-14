//! Minimal bencode codec for the pod protocol.
//!
//! The protocol only needs byte strings, lists, dicts and integers. Decoding
//! reads exactly one value from a stream so messages can be framed without
//! buffering the whole input.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read};

/// Maximum nesting depth accepted when decoding.
const MAX_DEPTH: usize = 64;

/// A bencode value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(BTreeMap<Vec<u8>, Value>),
}

impl Value {
    /// Builds a dict from string keys.
    pub fn dict<I, K>(pairs: I) -> Value
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<Vec<u8>>,
    {
        Value::Dict(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Returns the value as UTF-8 text, if it is a byte string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    /// Looks up a key in a dict.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.get(key.as_bytes()),
            _ => None,
        }
    }

    /// Looks up a key in a dict and returns it as text.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Encodes the value, appending to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Value::Int(i) => {
                out.push(b'i');
                out.extend_from_slice(i.to_string().as_bytes());
                out.push(b'e');
            }
            Value::Bytes(b) => encode_bytes(b, out),
            Value::List(items) => {
                out.push(b'l');
                for item in items {
                    item.encode(out);
                }
                out.push(b'e');
            }
            Value::Dict(d) => {
                out.push(b'd');
                for (k, v) in d {
                    encode_bytes(k, out);
                    v.encode(out);
                }
                out.push(b'e');
            }
        }
    }

    /// Encodes the value into a new buffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Bytes(s.as_bytes().to_vec())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Bytes(s.into_bytes())
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Value::List(items)
    }
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

/// Reads one value from `r`.
///
/// Returns `Ok(None)` when the stream ends cleanly before the first byte of a
/// value. End of stream inside a value is an `UnexpectedEof` error.
pub fn read_value<R: BufRead>(r: &mut R) -> io::Result<Option<Value>> {
    match read_byte(r)? {
        None => Ok(None),
        Some(first) => parse(r, first, 0).map(Some),
    }
}

fn read_byte<R: BufRead>(r: &mut R) -> io::Result<Option<u8>> {
    loop {
        let buf = match r.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let Some(&b) = buf.first() else {
            return Ok(None);
        };
        r.consume(1);
        return Ok(Some(b));
    }
}

fn next_byte<R: BufRead>(r: &mut R) -> io::Result<u8> {
    read_byte(r)?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated bencode"))
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn parse<R: BufRead>(r: &mut R, first: u8, depth: usize) -> io::Result<Value> {
    if depth > MAX_DEPTH {
        return Err(invalid("bencode nested too deeply"));
    }
    match first {
        b'i' => {
            let digits = read_until(r, b'e')?;
            let text = std::str::from_utf8(&digits).map_err(|_| invalid("invalid integer"))?;
            text.parse()
                .map(Value::Int)
                .map_err(|_| invalid(format!("invalid integer: {text}")))
        }
        b'0'..=b'9' => parse_bytes(r, first).map(Value::Bytes),
        b'l' => {
            let mut items = Vec::new();
            loop {
                match next_byte(r)? {
                    b'e' => return Ok(Value::List(items)),
                    b => items.push(parse(r, b, depth + 1)?),
                }
            }
        }
        b'd' => {
            let mut dict = BTreeMap::new();
            loop {
                match next_byte(r)? {
                    b'e' => return Ok(Value::Dict(dict)),
                    b @ b'0'..=b'9' => {
                        let key = parse_bytes(r, b)?;
                        let value_first = next_byte(r)?;
                        let value = parse(r, value_first, depth + 1)?;
                        dict.insert(key, value);
                    }
                    b => return Err(invalid(format!("invalid dict key prefix: {:?}", b as char))),
                }
            }
        }
        b => Err(invalid(format!("invalid bencode prefix: {:?}", b as char))),
    }
}

fn read_until<R: BufRead>(r: &mut R, end: u8) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match next_byte(r)? {
            b if b == end => return Ok(out),
            b => {
                if out.len() >= 20 {
                    return Err(invalid("bencode length or integer too long"));
                }
                out.push(b)
            }
        }
    }
}

fn parse_bytes<R: BufRead>(r: &mut R, first: u8) -> io::Result<Vec<u8>> {
    let mut digits = vec![first];
    digits.extend(read_until(r, b':')?);
    let len: u64 = std::str::from_utf8(&digits)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| invalid("invalid byte string length"))?;
    let mut out = Vec::new();
    r.take(len).read_to_end(&mut out)?;
    if out.len() as u64 != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated bencode byte string",
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn roundtrip(v: Value) {
        let bytes = v.to_bytes();
        let mut r = Cursor::new(bytes);
        assert_eq!(read_value(&mut r).unwrap(), Some(v));
        assert_eq!(read_value(&mut r).unwrap(), None);
    }

    #[test]
    fn encodes_known_forms() {
        assert_eq!(Value::from("spam").to_bytes(), b"4:spam");
        assert_eq!(Value::Int(-3).to_bytes(), b"i-3e");
        assert_eq!(
            Value::List(vec!["a".into(), Value::Int(1)]).to_bytes(),
            b"l1:ai1ee"
        );
        // keys are sorted
        let d = Value::dict([("zz", Value::from("1")), ("aa", Value::from("2"))]);
        assert_eq!(d.to_bytes(), b"d2:aa1:22:zz1:1e");
    }

    #[test]
    fn roundtrips() {
        roundtrip(Value::Int(0));
        roundtrip(Value::Int(i64::MIN));
        roundtrip(Value::from(""));
        roundtrip(Value::Bytes(vec![0, 255, b':', b'e']));
        roundtrip(Value::dict([
            ("op", Value::from("invoke")),
            ("args", Value::from("[1,2]")),
            ("status", Value::List(vec!["done".into(), "error".into()])),
            ("nested", Value::dict([("x", Value::List(vec![]))])),
        ]));
    }

    #[test]
    fn reads_consecutive_messages() {
        let mut bytes = Value::dict([("op", Value::from("describe"))]).to_bytes();
        bytes.extend(Value::dict([("op", Value::from("shutdown"))]).to_bytes());
        let mut r = Cursor::new(bytes);
        let a = read_value(&mut r).unwrap().unwrap();
        let b = read_value(&mut r).unwrap().unwrap();
        assert_eq!(a.get_str("op"), Some("describe"));
        assert_eq!(b.get_str("op"), Some("shutdown"));
        assert!(read_value(&mut r).unwrap().is_none());
    }

    #[test]
    fn truncated_input_is_an_error() {
        for input in [&b"d2:op"[..], b"l1:a", b"10:abc", b"i12"] {
            let err = read_value(&mut Cursor::new(input)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{input:?}");
        }
    }

    #[test]
    fn garbage_is_an_error() {
        for input in [&b"x"[..], b"di1ei2ee", b"iabce"] {
            let err = read_value(&mut Cursor::new(input)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{input:?}");
        }
    }

    #[test]
    fn rejects_deep_nesting() {
        let input = vec![b'l'; MAX_DEPTH + 2];
        let err = read_value(&mut Cursor::new(input)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
