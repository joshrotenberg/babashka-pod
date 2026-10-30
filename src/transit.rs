//! The `transit+json` payload format.
//!
//! [Transit](https://github.com/cognitect/transit-format) encodes Clojure
//! values in JSON: keywords, symbols, sets, insts and tagged values that plain
//! JSON cannot express. This module translates between transit+json text and
//! two representations:
//!
//! - [`serde_json::Value`], so handlers registered with
//!   [`Namespace::function`](crate::Namespace::function) and the like keep
//!   using ordinary serde types. Clojure types collapse on the way in.
//! - [`Value`], which keeps every type transit carries, for handlers
//!   registered with [`Namespace::function_transit`](crate::Namespace::function_transit),
//!   [`stream_transit`](crate::Namespace::stream_transit) and
//!   [`stream_fn_transit`](crate::Namespace::stream_fn_transit). [`read`]
//!   decodes into it, and it serializes back to the same transit value.
//!
//! In more detail:
//!
//! - [`decode`] reads what babashka's transit writer produces, including the
//!   read cache. Keywords and symbols decode to strings, sets and lists to
//!   arrays, insts to epoch milliseconds, so they deserialize into plain Rust
//!   types. Byte arrays, `with-meta` and unknown tags decode to marker
//!   objects that [`Bytes`], [`WithMeta`] and [`Tagged`] deserialize from.
//! - [`encode`] writes a value. Maps are written with keyword keys, matching
//!   the JSON format, where babashka keywordizes keys. The [`Keyword`],
//!   [`Symbol`], [`Set`], [`Inst`], [`Uuid`], [`Bytes`], [`Tagged`] and [`WithMeta`]
//!   types serialize to marker objects that `encode` writes as the
//!   corresponding transit values.
//!
//! The module depends only on serde and serde_json.

use std::fmt;

use serde::de::{self, DeserializeOwned, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value as Json};

/// Key of the single-entry object that marks a transit value inside a
/// `serde_json::Value`. The NUL byte keeps it from colliding with real keys.
const MARKER: &str = "\u{0}transit";

/// Entries in the read cache before it wraps around (44 * 44).
const CACHE_SIZE: usize = 1936;

/// Integers outside this range are written as `~i` strings, as the transit
/// spec requires for JSON.
const MAX_JSON_INT: u64 = 1 << 53;

/// A transit decoding error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid transit: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Decodes transit+json text into a `serde_json::Value`.
///
/// # Errors
///
/// Fails when the text is not JSON, refers to a cache entry not seen yet, or
/// holds a malformed transit value (an odd-length map, a bad scalar).
pub fn decode(text: &str) -> Result<Json, DecodeError> {
    let json: Json = serde_json::from_str(text).map_err(|e| DecodeError(e.to_string()))?;
    Reader::new().value(&json)
}

struct Reader {
    cache: Vec<String>,
    next: usize,
}

impl Reader {
    fn new() -> Self {
        Reader {
            cache: Vec::new(),
            next: 0,
        }
    }

    /// Resolves a cache reference, or records a cacheable string, and returns
    /// the encoded string itself.
    fn resolve(&mut self, s: &str, as_key: bool) -> Result<String, DecodeError> {
        if s.starts_with('^') && s != "^ " {
            let index = cache_index(&s[1..])
                .ok_or_else(|| DecodeError(format!("bad cache reference {s:?}")))?;
            return self
                .cache
                .get(index)
                .cloned()
                .ok_or_else(|| DecodeError(format!("cache reference {s:?} not seen yet")));
        }
        if is_cacheable(s, as_key) {
            if self.next == CACHE_SIZE {
                self.next = 0;
            }
            if self.next < self.cache.len() {
                self.cache[self.next] = s.to_string();
            } else {
                self.cache.push(s.to_string());
            }
            self.next += 1;
        }
        Ok(s.to_string())
    }

    fn value(&mut self, v: &Json) -> Result<Json, DecodeError> {
        match v {
            Json::String(s) => {
                let raw = self.resolve(s, false)?;
                scalar(&raw)
            }
            Json::Array(items) => self.array(items),
            Json::Object(obj) => self.verbose_map(obj),
            other => Ok(other.clone()),
        }
    }

    fn array(&mut self, items: &[Json]) -> Result<Json, DecodeError> {
        let Some(Json::String(first)) = items.first() else {
            return items.iter().map(|v| self.value(v)).collect();
        };
        let head = self.resolve(first, false)?;
        if head == "^ " {
            let rest = &items[1..];
            if rest.len() % 2 != 0 {
                return Err(DecodeError("map with an odd number of entries".into()));
            }
            let mut map = Map::new();
            for pair in rest.chunks(2) {
                let key = self.key(&pair[0])?;
                let value = self.value(&pair[1])?;
                map.insert(key, value);
            }
            return Ok(Json::Object(map));
        }
        if let Some(tag) = head.strip_prefix("~#") {
            if items.len() != 2 {
                return Err(DecodeError(format!("tag {tag} without exactly one value")));
            }
            return self.tagged(tag, &items[1]);
        }
        let mut out = vec![scalar(&head)?];
        for v in &items[1..] {
            out.push(self.value(v)?);
        }
        Ok(Json::Array(out))
    }

    /// A map written as a JSON object (transit's verbose mode), or a verbose
    /// tagged value `{"~#tag": rep}`.
    fn verbose_map(&mut self, obj: &Map<String, Json>) -> Result<Json, DecodeError> {
        if obj.len() == 1 {
            let (k, v) = obj.iter().next().unwrap();
            if let Some(tag) = k.strip_prefix("~#") {
                return self.tagged(tag, v);
            }
        }
        let mut map = Map::new();
        for (k, v) in obj {
            let key = self.key(&Json::String(k.clone()))?;
            map.insert(key, self.value(v)?);
        }
        Ok(Json::Object(map))
    }

    fn key(&mut self, k: &Json) -> Result<String, DecodeError> {
        let decoded = match k {
            Json::String(s) => {
                let raw = self.resolve(s, true)?;
                scalar(&raw)?
            }
            other => self.value(other)?,
        };
        Ok(match decoded {
            Json::String(s) => s,
            other => other.to_string(),
        })
    }

    fn tagged(&mut self, tag: &str, rep: &Json) -> Result<Json, DecodeError> {
        match tag {
            "set" | "list" => match self.value(rep)? {
                arr @ Json::Array(_) => Ok(arr),
                other => Err(DecodeError(format!("{tag} of a non-array: {other}"))),
            },
            // A map whose keys are not all strings: decoded to [[k v] ...].
            "cmap" => {
                let Json::Array(flat) = self.value(rep)? else {
                    return Err(DecodeError("cmap of a non-array".into()));
                };
                Ok(Json::Array(
                    flat.chunks(2).map(|kv| Json::Array(kv.to_vec())).collect(),
                ))
            }
            "with-meta" => {
                let Json::Array(pair) = self.value(rep)? else {
                    return Err(DecodeError("with-meta of a non-array".into()));
                };
                let [value, meta] = <[Json; 2]>::try_from(pair)
                    .map_err(|_| DecodeError("with-meta needs [value meta]".into()))?;
                Ok(marker("with-meta", vec![value, meta]))
            }
            "ratio" => {
                let Json::Array(nd) = self.value(rep)? else {
                    return Err(DecodeError("ratio of a non-array".into()));
                };
                let (n, d) = match nd.as_slice() {
                    [n, d] => (as_f64(n), as_f64(d)),
                    _ => (None, None),
                };
                match (n, d) {
                    (Some(n), Some(d)) => Ok(Number::from_f64(n / d)
                        .map(Json::Number)
                        .unwrap_or(Json::Null)),
                    _ => Err(DecodeError("ratio needs [numerator denominator]".into())),
                }
            }
            "'" => self.value(rep),
            other => {
                let rep = self.value(rep)?;
                Ok(marker("tagged", vec![Json::from(other), rep]))
            }
        }
    }
}

fn as_f64(v: &Json) -> Option<f64> {
    match v {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn is_cacheable(s: &str, as_key: bool) -> bool {
    s.len() > 3 && (as_key || s.starts_with("~:") || s.starts_with("~$") || s.starts_with("~#"))
}

/// Cache codes are one or two base-44 digits starting at `'0'`.
fn cache_index(code: &str) -> Option<usize> {
    let digit = |b: u8| (48..48 + 44).contains(&b).then(|| (b - 48) as usize);
    match code.as_bytes() {
        [a] => digit(*a),
        [a, b] => Some(digit(*a)? * 44 + digit(*b)?),
        _ => None,
    }
}

/// Decodes a non-cache transit string.
fn scalar(raw: &str) -> Result<Json, DecodeError> {
    let mut chars = raw.chars();
    if chars.next() != Some('~') {
        return Ok(Json::String(raw.to_string()));
    }
    let Some(kind) = chars.next() else {
        return Ok(Json::String(raw.to_string()));
    };
    let body = &raw[2..];
    let bad = || DecodeError(format!("bad transit scalar {raw:?}"));
    Ok(match kind {
        '~' | '^' | '`' => Json::String(raw[1..].to_string()),
        ':' | '$' | 'u' | 'r' | 'c' | 'n' | 'f' | 't' => Json::String(body.to_string()),
        // Kept as a marker so a blob is not mistaken for text.
        'b' => marker("bytes", vec![Json::String(body.to_string())]),
        'm' => Json::from(body.parse::<i64>().map_err(|_| bad())?),
        'i' => match (body.parse::<i64>(), body.parse::<u64>()) {
            (Ok(n), _) => Json::from(n),
            (_, Ok(n)) => Json::from(n),
            _ => Json::String(body.to_string()),
        },
        'd' => body
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Json::Number)
            .ok_or_else(bad)?,
        '?' => match body {
            "t" => Json::Bool(true),
            "f" => Json::Bool(false),
            _ => return Err(bad()),
        },
        '_' => Json::Null,
        // NaN and infinities have no JSON number form.
        'z' => Json::String(body.to_string()),
        _ => Json::String(raw.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Encodes a `serde_json::Value`, including marker objects from this
/// module's types, as transit+json text.
pub fn encode(v: &Json) -> String {
    encode_value(v).to_string()
}

fn encode_value(v: &Json) -> Json {
    match v {
        Json::String(s) => Json::String(escape(s)),
        Json::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) if i.unsigned_abs() > MAX_JSON_INT => Json::String(format!("~i{i}")),
            (None, Some(u)) if u > MAX_JSON_INT => Json::String(format!("~i{u}")),
            _ => v.clone(),
        },
        Json::Array(items) => Json::Array(items.iter().map(encode_value).collect()),
        Json::Object(obj) => match as_marker(obj) {
            Some((kind, args)) => encode_marker(kind, args),
            None => {
                let mut out = vec![Json::from("^ ")];
                for (k, v) in obj {
                    out.push(Json::String(format!("~:{k}")));
                    out.push(encode_value(v));
                }
                Json::Array(out)
            }
        },
        other => other.clone(),
    }
}

fn escape(s: &str) -> String {
    if s.starts_with(['~', '^', '`']) {
        format!("~{s}")
    } else {
        s.to_string()
    }
}

fn encode_marker(kind: &str, args: &[Json]) -> Json {
    let text = |i: usize| args.get(i).and_then(Json::as_str).unwrap_or_default();
    let tagged = |tag: &str, rep: Json| Json::Array(vec![Json::String(format!("~#{tag}")), rep]);
    match kind {
        "keyword" => Json::String(format!("~:{}", text(0))),
        "symbol" => Json::String(format!("~${}", text(0))),
        "uuid" => Json::String(format!("~u{}", text(0))),
        "bytes" => Json::String(format!("~b{}", text(0))),
        "inst" => Json::String(format!(
            "~m{}",
            args.first().and_then(Json::as_i64).unwrap_or_default()
        )),
        "set" => tagged("set", encode_value(args.first().unwrap_or(&Json::Null))),
        "tagged" => tagged(text(0), encode_value(args.get(1).unwrap_or(&Json::Null))),
        "with-meta" => tagged(
            "with-meta",
            Json::Array(args.iter().take(2).map(encode_value).collect()),
        ),
        // A transit::Value: [transit, plain JSON].
        "raw" => args.first().cloned().unwrap_or(Json::Null),
        _ => Json::Null,
    }
}

/// Rewrites marker objects as plain JSON, for pods that use the JSON format:
/// keywords, symbols and uuids become strings, bytes become base64 strings,
/// sets become arrays, insts become epoch milliseconds, tagged values become `{"tag": .., "rep": ..}`,
/// and `with-meta` drops the metadata.
pub fn to_plain_json(v: &Json) -> Json {
    match v {
        Json::Array(items) => Json::Array(items.iter().map(to_plain_json).collect()),
        Json::Object(obj) => match as_marker(obj) {
            Some((kind, args)) => {
                let arg = |i: usize| args.get(i).map(to_plain_json).unwrap_or(Json::Null);
                match kind {
                    "tagged" => serde_json::json!({ "tag": arg(0), "rep": arg(1) }),
                    // A transit::Value: [transit, plain JSON].
                    "raw" => args.get(1).cloned().unwrap_or(Json::Null),
                    _ => arg(0),
                }
            }
            None => Json::Object(
                obj.iter()
                    .map(|(k, v)| (k.clone(), to_plain_json(v)))
                    .collect(),
            ),
        },
        other => other.clone(),
    }
}

fn marker(kind: &str, args: Vec<Json>) -> Json {
    let mut items = vec![Json::from(kind)];
    items.extend(args);
    let mut map = Map::new();
    map.insert(MARKER.to_string(), Json::Array(items));
    Json::Object(map)
}

fn as_marker(obj: &Map<String, Json>) -> Option<(&str, &[Json])> {
    if obj.len() != 1 {
        return None;
    }
    let Json::Array(items) = obj.get(MARKER)? else {
        return None;
    };
    let (kind, args) = items.split_first()?;
    Some((kind.as_str()?, args))
}

// ---------------------------------------------------------------------------
// Marker types
// ---------------------------------------------------------------------------

/// Serializes `{MARKER: [kind, args...]}`. `items` is a tuple whose first
/// element is the kind, so it serializes as one array.
fn serialize_marker<S: Serializer, T: Serialize>(s: S, items: &T) -> Result<S::Ok, S::Error> {
    let mut map = s.serialize_map(Some(1))?;
    map.serialize_entry(MARKER, items)?;
    map.end()
}

/// A Clojure keyword. The name has no leading colon: `Keyword::new("a")` is
/// `:a`, `Keyword::new("ns/a")` is `:ns/a`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Keyword(pub String);

impl Keyword {
    /// Creates a keyword from its name, without the colon.
    pub fn new(name: impl Into<String>) -> Self {
        Keyword(name.into())
    }
}

impl Serialize for Keyword {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("keyword", &self.0))
    }
}

impl<'de> Deserialize<'de> for Keyword {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(|s| Keyword(s.strip_prefix(':').unwrap_or(&s).to_string()))
    }
}

/// A Clojure symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(pub String);

impl Symbol {
    /// Creates a symbol from its name.
    pub fn new(name: impl Into<String>) -> Self {
        Symbol(name.into())
    }
}

impl Serialize for Symbol {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("symbol", &self.0))
    }
}

impl<'de> Deserialize<'de> for Symbol {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Symbol)
    }
}

/// A Clojure set. Order is not significant; the elements are kept as given.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Set<T>(pub Vec<T>);

impl<T: Serialize> Serialize for Set<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("set", &self.0))
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Set<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::deserialize(d).map(Set)
    }
}

impl<T> FromIterator<T> for Set<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Set(iter.into_iter().collect())
    }
}

/// A point in time (`#inst`), as milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Inst(pub i64);

impl Inst {
    /// The current time.
    pub fn now() -> Self {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        Inst(millis)
    }
}

impl Serialize for Inst {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("inst", self.0))
    }
}

impl<'de> Deserialize<'de> for Inst {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        i64::deserialize(d).map(Inst)
    }
}

/// A UUID (`#uuid`), in its canonical string form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Uuid(pub String);

impl Serialize for Uuid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("uuid", &self.0))
    }
}

impl<'de> Deserialize<'de> for Uuid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Uuid)
    }
}

/// A byte array: a BLOB, a file's contents. babashka sees a `byte[]`.
///
/// Travels as base64 (transit `~b`, or a plain base64 string under the JSON
/// format).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    /// Reads bytes from a decoded argument that may hold them, for handlers
    /// that take untyped `serde_json::Value` arguments. Only the transit byte
    /// marker matches; a plain string is left as text.
    pub fn from_json(v: &Json) -> Option<Bytes> {
        match v.as_object().and_then(as_marker) {
            Some(("bytes", [Json::String(b64)])) => base64_decode(b64).map(Bytes),
            _ => None,
        }
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("bytes", base64_encode(&self.0)))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Json::deserialize(d)?;
        let b64 = match (&v, v.as_object().and_then(as_marker)) {
            (_, Some(("bytes", [Json::String(b64)]))) => b64.as_str(),
            (Json::String(b64), _) => b64.as_str(),
            _ => return Err(de::Error::custom(format!("expected bytes, got {v}"))),
        };
        base64_decode(b64)
            .map(Bytes)
            .ok_or_else(|| de::Error::custom("invalid base64"))
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A transit tagged value with a tag the decoder does not know, or one to
/// send to babashka, where a read handler registered with
/// `babashka.pods/add-transit-read-handler!` turns it into a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Tagged {
    /// The tag, without `~#`.
    pub tag: String,
    /// The value's representation.
    pub rep: Json,
}

impl Serialize for Tagged {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("tagged", &self.tag, &self.rep))
    }
}

impl<'de> Deserialize<'de> for Tagged {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Json::deserialize(d)?;
        match v.as_object().and_then(as_marker) {
            Some(("tagged", [Json::String(tag), rep])) => Ok(Tagged {
                tag: tag.clone(),
                rep: rep.clone(),
            }),
            _ => Err(de::Error::custom(format!(
                "expected a tagged value, got {v}"
            ))),
        }
    }
}

/// A value with optional metadata.
///
/// For a var registered with [`Namespace::arg_meta`](crate::Namespace::arg_meta),
/// babashka sends arguments that carry metadata wrapped in `with-meta`; use
/// `WithMeta<T>` for such an argument. Arguments without metadata arrive
/// plain and get `meta: None`.
#[derive(Debug, Clone, PartialEq)]
pub struct WithMeta<T> {
    /// The value.
    pub value: T,
    /// Its metadata map, if it had any.
    pub meta: Option<Json>,
}

impl<T: Serialize> Serialize for WithMeta<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.meta {
            Some(meta) => serialize_marker(s, &("with-meta", &self.value, meta)),
            None => self.value.serialize(s),
        }
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for WithMeta<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Json::deserialize(d)?;
        if let Some(("with-meta", [value, meta])) = v.as_object().and_then(as_marker) {
            let value = serde_json::from_value(value.clone()).map_err(de::Error::custom)?;
            return Ok(WithMeta {
                value,
                meta: Some(meta.clone()),
            });
        }
        let value = serde_json::from_value(v).map_err(de::Error::custom)?;
        Ok(WithMeta { value, meta: None })
    }
}

// ---------------------------------------------------------------------------
// Lossless values
// ---------------------------------------------------------------------------

/// A Clojure value as transit carries it, with nothing collapsed.
///
/// [`decode`] maps transit onto `serde_json::Value`, so keywords arrive as
/// strings, lists and sets as arrays, and ratios as floats. `Value` keeps each
/// type distinct: a keyword is not a string, a list is not a vector, `1/3` is
/// a ratio, and a map's keys can be anything, so `{:a 1 "a" 2}` keeps both
/// entries. Handlers registered with
/// [`Namespace::function_transit`](crate::Namespace::function_transit) and
/// the other `*_transit` methods get their arguments as `Value`s.
///
/// `Value` implements [`Serialize`]: returned from a handler, or inside a
/// returned value, it is written back as the same transit value. Under the
/// JSON format it is written as plain JSON, as [`decode`] would read it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// `nil`.
    Nil,
    /// `true` or `false`.
    Bool(bool),
    /// A long.
    Int(i64),
    /// A `BigInt` (`1N`), as a decimal string.
    BigInt(String),
    /// A double. NaN and the infinities are kept.
    Float(f64),
    /// A `BigDecimal` (`1.5M`), as a decimal string.
    BigDec(String),
    /// A ratio (`1/3`), with numerator and denominator as decimal strings.
    Ratio {
        /// The numerator.
        numerator: String,
        /// The denominator.
        denominator: String,
    },
    /// A string.
    String(String),
    /// A character (`\x`).
    Char(char),
    /// A keyword, without the colon: `Keyword("ns/a".into())` is `:ns/a`.
    Keyword(String),
    /// A symbol.
    Symbol(String),
    /// An `#inst`, as milliseconds since the Unix epoch.
    Inst(i64),
    /// A `#uuid`, in its canonical string form.
    Uuid(String),
    /// A `java.net.URI`.
    Uri(String),
    /// A byte array.
    Bytes(Vec<u8>),
    /// A list.
    List(Vec<Value>),
    /// A vector.
    Vector(Vec<Value>),
    /// A set, with elements in the order they were read.
    Set(Vec<Value>),
    /// A map, as key-value pairs in the order they were read.
    Map(Vec<(Value, Value)>),
    /// A tagged value with a tag this module does not know: tag (without
    /// `~#`) and representation.
    Tagged(String, Box<Value>),
    /// A value and its metadata map.
    WithMeta(Box<Value>, Box<Value>),
}

impl Value {
    /// The value for `key` in a map, or `None` for a missing key or a value
    /// that is not a map.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_marker(s, &("raw", write(self), plain(self)))
    }
}

/// Builds a `Value` from plain JSON, which carries no Clojure types: objects
/// become maps with string keys and arrays become vectors.
impl From<&Json> for Value {
    fn from(v: &Json) -> Self {
        match v {
            Json::Null => Value::Nil,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => number(n),
            Json::String(s) => Value::String(s.clone()),
            Json::Array(items) => Value::Vector(items.iter().map(Value::from).collect()),
            Json::Object(obj) => Value::Map(
                obj.iter()
                    .map(|(k, v)| (Value::String(k.clone()), Value::from(v)))
                    .collect(),
            ),
        }
    }
}

fn number(n: &Number) -> Value {
    match (n.as_i64(), n.as_u64(), n.as_f64()) {
        (Some(i), _, _) => Value::Int(i),
        (None, Some(u), _) => Value::BigInt(u.to_string()),
        (_, _, Some(f)) => Value::Float(f),
        _ => Value::BigDec(n.to_string()),
    }
}

/// Decodes transit+json text into a [`Value`], keeping every type transit
/// carries.
///
/// # Errors
///
/// Fails like [`decode`], and also on a char that is not one character or a
/// ratio of non-integers.
pub fn read(text: &str) -> Result<Value, DecodeError> {
    let json: Json = serde_json::from_str(text).map_err(|e| DecodeError(e.to_string()))?;
    Reader::new().tree(&json)
}

impl Reader {
    fn tree(&mut self, v: &Json) -> Result<Value, DecodeError> {
        match v {
            Json::Null => Ok(Value::Nil),
            Json::Bool(b) => Ok(Value::Bool(*b)),
            Json::Number(n) => Ok(number(n)),
            Json::String(s) => {
                let raw = self.resolve(s, false)?;
                tree_scalar(&raw)
            }
            Json::Array(items) => self.tree_array(items),
            Json::Object(obj) => self.tree_verbose_map(obj),
        }
    }

    fn tree_items(&mut self, items: &[Json]) -> Result<Vec<Value>, DecodeError> {
        items.iter().map(|v| self.tree(v)).collect()
    }

    fn tree_array(&mut self, items: &[Json]) -> Result<Value, DecodeError> {
        let Some(Json::String(first)) = items.first() else {
            return self.tree_items(items).map(Value::Vector);
        };
        let head = self.resolve(first, false)?;
        if head == "^ " {
            let rest = &items[1..];
            if rest.len() % 2 != 0 {
                return Err(DecodeError("map with an odd number of entries".into()));
            }
            let mut entries = Vec::with_capacity(rest.len() / 2);
            for pair in rest.chunks(2) {
                let key = self.tree_key(&pair[0])?;
                entries.push((key, self.tree(&pair[1])?));
            }
            return Ok(Value::Map(entries));
        }
        if let Some(tag) = head.strip_prefix("~#") {
            if items.len() != 2 {
                return Err(DecodeError(format!("tag {tag} without exactly one value")));
            }
            return self.tree_tagged(tag, &items[1]);
        }
        let mut out = vec![tree_scalar(&head)?];
        for v in &items[1..] {
            out.push(self.tree(v)?);
        }
        Ok(Value::Vector(out))
    }

    fn tree_verbose_map(&mut self, obj: &Map<String, Json>) -> Result<Value, DecodeError> {
        if obj.len() == 1 {
            let (k, v) = obj.iter().next().unwrap();
            if let Some(tag) = k.strip_prefix("~#") {
                return self.tree_tagged(tag, v);
            }
        }
        let mut entries = Vec::with_capacity(obj.len());
        for (k, v) in obj {
            let key = self.tree_key(&Json::String(k.clone()))?;
            entries.push((key, self.tree(v)?));
        }
        Ok(Value::Map(entries))
    }

    fn tree_key(&mut self, k: &Json) -> Result<Value, DecodeError> {
        match k {
            Json::String(s) => {
                let raw = self.resolve(s, true)?;
                tree_scalar(&raw)
            }
            other => self.tree(other),
        }
    }

    fn tree_tagged(&mut self, tag: &str, rep: &Json) -> Result<Value, DecodeError> {
        let items = |v: Value| match v {
            Value::Vector(items) => Ok(items),
            other => Err(DecodeError(format!("{tag} of a non-array: {other:?}"))),
        };
        match tag {
            "set" => Ok(Value::Set(items(self.tree(rep)?)?)),
            "list" => Ok(Value::List(items(self.tree(rep)?)?)),
            "cmap" => {
                let flat = items(self.tree(rep)?)?;
                if flat.len() % 2 != 0 {
                    return Err(DecodeError("cmap with an odd number of entries".into()));
                }
                let mut entries = Vec::with_capacity(flat.len() / 2);
                let mut it = flat.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    entries.push((k, v));
                }
                Ok(Value::Map(entries))
            }
            "with-meta" => match <[Value; 2]>::try_from(items(self.tree(rep)?)?) {
                Ok([value, meta]) => Ok(Value::WithMeta(Box::new(value), Box::new(meta))),
                Err(_) => Err(DecodeError("with-meta needs [value meta]".into())),
            },
            "ratio" => {
                let integer = |v: &Value| match v {
                    Value::Int(i) => Some(i.to_string()),
                    Value::BigInt(s) => Some(s.clone()),
                    _ => None,
                };
                match items(self.tree(rep)?)?.as_slice() {
                    [n, d] => match (integer(n), integer(d)) {
                        (Some(numerator), Some(denominator)) => Ok(Value::Ratio {
                            numerator,
                            denominator,
                        }),
                        _ => Err(DecodeError("ratio of non-integers".into())),
                    },
                    _ => Err(DecodeError("ratio needs [numerator denominator]".into())),
                }
            }
            "'" => self.tree(rep),
            other => Ok(Value::Tagged(other.to_string(), Box::new(self.tree(rep)?))),
        }
    }
}

/// Decodes a non-cache transit string into a [`Value`].
fn tree_scalar(raw: &str) -> Result<Value, DecodeError> {
    let mut chars = raw.chars();
    if chars.next() != Some('~') {
        return Ok(Value::String(raw.to_string()));
    }
    let Some(kind) = chars.next() else {
        return Ok(Value::String(raw.to_string()));
    };
    let body = &raw[2..];
    let bad = || DecodeError(format!("bad transit scalar {raw:?}"));
    Ok(match kind {
        '~' | '^' | '`' => Value::String(raw[1..].to_string()),
        ':' => Value::Keyword(body.to_string()),
        '$' => Value::Symbol(body.to_string()),
        'u' => Value::Uuid(body.to_string()),
        'r' => Value::Uri(body.to_string()),
        'c' => {
            let mut cs = body.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => Value::Char(c),
                _ => return Err(bad()),
            }
        }
        'n' => Value::BigInt(body.to_string()),
        'f' => Value::BigDec(body.to_string()),
        // A verbose-mode timestamp, kept as its text as decode does.
        't' => Value::String(body.to_string()),
        'b' => Value::Bytes(base64_decode(body).ok_or_else(bad)?),
        'm' => Value::Inst(body.parse::<i64>().map_err(|_| bad())?),
        'i' => match body.parse::<i64>() {
            Ok(n) => Value::Int(n),
            Err(_) => Value::BigInt(body.to_string()),
        },
        'd' => Value::Float(body.parse::<f64>().map_err(|_| bad())?),
        '?' => match body {
            "t" => Value::Bool(true),
            "f" => Value::Bool(false),
            _ => return Err(bad()),
        },
        '_' => Value::Nil,
        'z' => match body {
            "NaN" => Value::Float(f64::NAN),
            "INF" => Value::Float(f64::INFINITY),
            "-INF" => Value::Float(f64::NEG_INFINITY),
            _ => return Err(bad()),
        },
        _ => Value::String(raw.to_string()),
    })
}

/// A [`Value`] as plain JSON, the way [`decode`] reads the same transit and
/// [`to_plain_json`] writes the marker types: keywords and symbols become
/// their names, big numbers and special floats strings, ratios floats, bytes
/// base64, collections arrays, tagged values `{"tag": .., "rep": ..}`, and
/// metadata is dropped.
fn plain(v: &Value) -> Json {
    let text = |s: &str| Json::String(s.to_string());
    let all = |items: &[Value]| Json::Array(items.iter().map(plain).collect());
    match v {
        Value::Nil => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Float(f) if f.is_nan() => text("NaN"),
        Value::Float(f) if f.is_infinite() => text(if *f > 0.0 { "INF" } else { "-INF" }),
        Value::Float(f) => Number::from_f64(*f).map(Json::Number).unwrap_or(Json::Null),
        Value::Ratio {
            numerator,
            denominator,
        } => match (numerator.parse::<f64>(), denominator.parse::<f64>()) {
            (Ok(n), Ok(d)) => Number::from_f64(n / d)
                .map(Json::Number)
                .unwrap_or(Json::Null),
            _ => Json::Null,
        },
        Value::BigInt(s)
        | Value::BigDec(s)
        | Value::String(s)
        | Value::Keyword(s)
        | Value::Symbol(s)
        | Value::Uuid(s)
        | Value::Uri(s) => text(s),
        Value::Char(c) => Json::String(c.to_string()),
        Value::Inst(ms) => Json::from(*ms),
        Value::Bytes(b) => Json::String(base64_encode(b)),
        Value::List(items) | Value::Vector(items) | Value::Set(items) => all(items),
        Value::Map(entries) => Json::Object(
            entries
                .iter()
                .map(|(k, v)| {
                    let key = match plain(k) {
                        Json::String(s) => s,
                        other => other.to_string(),
                    };
                    (key, plain(v))
                })
                .collect(),
        ),
        Value::Tagged(tag, rep) => serde_json::json!({ "tag": tag, "rep": plain(rep) }),
        Value::WithMeta(value, _) => plain(value),
    }
}

/// Writes a [`Value`] as transit, in the JSON form [`encode`] produces.
fn write(v: &Value) -> Json {
    let text = |s: String| Json::String(s);
    let tagged = |tag: &str, rep: Json| Json::Array(vec![text(format!("~#{tag}")), rep]);
    let all = |items: &[Value]| Json::Array(items.iter().map(write).collect());
    match v {
        Value::Nil => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) if i.unsigned_abs() > MAX_JSON_INT => text(format!("~i{i}")),
        Value::Int(i) => Json::from(*i),
        Value::BigInt(n) => text(format!("~n{n}")),
        Value::Float(f) if f.is_nan() => text("~zNaN".into()),
        Value::Float(f) if f.is_infinite() => {
            text(if *f > 0.0 { "~zINF" } else { "~z-INF" }.into())
        }
        Value::Float(f) => Number::from_f64(*f).map(Json::Number).unwrap_or(Json::Null),
        Value::BigDec(d) => text(format!("~f{d}")),
        Value::Ratio {
            numerator,
            denominator,
        } => tagged(
            "ratio",
            Json::Array(vec![
                text(format!("~n{numerator}")),
                text(format!("~n{denominator}")),
            ]),
        ),
        Value::String(s) => text(escape(s)),
        Value::Char(c) => text(format!("~c{c}")),
        Value::Keyword(k) => text(format!("~:{k}")),
        Value::Symbol(s) => text(format!("~${s}")),
        Value::Inst(ms) => text(format!("~m{ms}")),
        Value::Uuid(u) => text(format!("~u{u}")),
        Value::Uri(u) => text(format!("~r{u}")),
        Value::Bytes(b) => text(format!("~b{}", base64_encode(b))),
        Value::List(items) => tagged("list", all(items)),
        Value::Vector(items) => all(items),
        Value::Set(items) => tagged("set", all(items)),
        Value::Map(entries) => {
            let flat: Vec<Json> = entries
                .iter()
                .flat_map(|(k, v)| [write(k), write(v)])
                .collect();
            // Keys written as strings fit a "^ " map; others need a cmap.
            if flat.iter().step_by(2).all(Json::is_string) {
                let mut out = vec![Json::from("^ ")];
                out.extend(flat);
                Json::Array(out)
            } else {
                tagged("cmap", Json::Array(flat))
            }
        }
        Value::Tagged(tag, rep) => tagged(tag, write(rep)),
        Value::WithMeta(value, meta) => {
            tagged("with-meta", Json::Array(vec![write(value), write(meta)]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Inputs below were produced by babashka's cognitect transit writer.

    #[test]
    fn decodes_scalars_keywords_and_maps() {
        assert_eq!(
            decode(r#"[1,"two","~:three"]"#).unwrap(),
            json!([1, "two", "three"])
        );
        assert_eq!(
            decode(r#"["^ ","~:a",1,"~:b","x"]"#).unwrap(),
            json!({"a": 1, "b": "x"})
        );
        assert_eq!(
            decode(r#"["^ ","str-key",1]"#).unwrap(),
            json!({"str-key": 1})
        );
    }

    #[test]
    fn decodes_cache_references_for_keys_and_tags() {
        let text = r#"[["^ ","~:name","a","~:tags",["~#set",["~:y","~:x"]]],["^ ","^0","b","^1",["^2",["~:x"]]]]"#;
        assert_eq!(
            decode(text).unwrap(),
            json!([{"name": "a", "tags": ["y", "x"]}, {"name": "b", "tags": ["x"]}])
        );
    }

    #[test]
    fn decodes_insts_uuids_symbols_and_escapes() {
        assert_eq!(
            decode(r#"["~m1791936000000","~u550e8400-e29b-41d4-a716-446655440000","~$sym","~$ns/sym","~:ns/kw"]"#).unwrap(),
            json!([1791936000000_i64, "550e8400-e29b-41d4-a716-446655440000", "sym", "ns/sym", "ns/kw"])
        );
        assert_eq!(
            decode(r#"["~~tilde","~^caret","~`tick",null,true,2.5,"~i9007199254740993"]"#).unwrap(),
            json!([
                "~tilde",
                "^caret",
                "`tick",
                null,
                true,
                2.5,
                9007199254740993_u64
            ])
        );
    }

    #[test]
    fn decodes_lists_composite_maps_and_with_meta() {
        assert_eq!(decode(r#"["~#list",[1,2]]"#).unwrap(), json!([1, 2]));
        assert_eq!(
            decode(r#"["~#cmap",[[1,2],"~:composite"]]"#).unwrap(),
            json!([[[1, 2], "composite"]])
        );
        let v = decode(r#"[["~#with-meta",[["^ ","~:a",1],["^ ","~:tag","~:x"]]],2]"#).unwrap();
        let (first, second): (WithMeta<Json>, WithMeta<i64>) = serde_json::from_value(v).unwrap();
        assert_eq!(first.value, json!({"a": 1}));
        assert_eq!(first.meta, Some(json!({"tag": "x"})));
        assert_eq!(
            second,
            WithMeta {
                value: 2,
                meta: None
            }
        );
    }

    #[test]
    fn unknown_tags_decode_to_tagged() {
        let v = decode(r#"["~#point",[1,2]]"#).unwrap();
        let t: Tagged = serde_json::from_value(v).unwrap();
        assert_eq!(
            t,
            Tagged {
                tag: "point".into(),
                rep: json!([1, 2])
            }
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode("not json").is_err());
        assert!(decode(r#"["^5"]"#).is_err(), "reference to an empty cache");
        assert!(decode(r#"["^ ","~:a"]"#).is_err(), "odd map");
    }

    #[test]
    fn bytes_round_trip_with_bb_fixtures() {
        // As bb writes [(.getBytes "hi!") (byte-array [0 255 16]) (byte-array [])].
        let v = decode(r#"["~baGkh","~bAP8Q","~b"]"#).unwrap();
        let bytes: Vec<Bytes> = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(
            bytes,
            [
                Bytes(b"hi!".to_vec()),
                Bytes(vec![0, 255, 16]),
                Bytes(vec![])
            ]
        );
        assert_eq!(Bytes::from_json(&v[1]), Some(Bytes(vec![0, 255, 16])));
        assert_eq!(
            Bytes::from_json(&json!("aGkh")),
            None,
            "plain text stays text"
        );
        assert_eq!(
            encode(&serde_json::to_value(&bytes).unwrap()),
            r#"["~baGkh","~bAP8Q","~b"]"#
        );
        assert_eq!(
            to_plain_json(&serde_json::to_value(&bytes).unwrap()),
            json!(["aGkh", "AP8Q", ""])
        );
        let from_json_format: Bytes = serde_json::from_value(json!("AP8Q")).unwrap();
        assert_eq!(from_json_format, Bytes(vec![0, 255, 16]));
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        for (raw, b64) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"\xff\xfe\x00", "//4A"),
        ] {
            assert_eq!(base64_encode(raw), b64);
            assert_eq!(base64_decode(b64).unwrap(), raw);
        }
        assert_eq!(base64_decode("not base64!"), None);
    }

    #[test]
    fn cache_codes_are_base_44() {
        assert_eq!(cache_index("0"), Some(0));
        assert_eq!(cache_index("1"), Some(1));
        assert_eq!(cache_index("10"), Some(44));
        assert_eq!(cache_index("[["), Some(43 * 44 + 43));
        assert_eq!(cache_index(""), None);
    }

    #[test]
    fn cache_wraps_after_1936_entries() {
        // 1937 distinct cacheable keywords, then a reference to slot 0, which
        // now holds the 1937th.
        let mut items: Vec<String> = (0..1937).map(|i| format!("\"~:k{i:04}\"")).collect();
        items.push("\"^0\"".to_string());
        let v = decode(&format!("[{}]", items.join(","))).unwrap();
        assert_eq!(v[1937], json!("k1936"));
    }

    #[test]
    fn encodes_maps_with_keyword_keys_and_escapes_strings() {
        assert_eq!(encode(&json!({"a": 1})), r#"["^ ","~:a",1]"#);
        assert_eq!(
            encode(&json!(["~x", "^y", "`z", "plain"])),
            r#"["~~x","~^y","~`z","plain"]"#
        );
        assert_eq!(
            encode(&json!(9007199254740993_u64)),
            r#""~i9007199254740993""#
        );
        assert_eq!(
            encode(&json!(-9007199254740993_i64)),
            r#""~i-9007199254740993""#
        );
        assert_eq!(encode(&json!(9007199254740992_u64)), "9007199254740992");
    }

    #[test]
    fn encodes_value_types() {
        let v = serde_json::to_value((
            Keyword::new("a"),
            Symbol::new("ns/f"),
            Set(vec![Keyword::new("x"), Keyword::new("y")]),
            Inst(1791936000000),
            Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
            Tagged {
                tag: "point".into(),
                rep: json!([1, 2]),
            },
            WithMeta {
                value: json!([1]),
                meta: Some(json!({"k": 1})),
            },
        ))
        .unwrap();
        assert_eq!(
            encode(&v),
            concat!(
                r#"["~:a","~$ns/f",["~#set",["~:x","~:y"]],"~m1791936000000","#,
                r#""~u550e8400-e29b-41d4-a716-446655440000",["~#point",[1,2]],"#,
                r#"["~#with-meta",[[1],["^ ","~:k",1]]]]"#
            )
        );
    }

    #[test]
    fn round_trips_through_plain_values() {
        let v = serde_json::to_value(json!({
            "name": "a",
            "tags": Set(vec![Keyword::new("x")]),
            "at": Inst(5),
            "nested": [{"deep": "~escaped"}],
        }))
        .unwrap();
        assert_eq!(decode(&encode(&v)).unwrap(), to_plain_json(&v));
    }

    #[test]
    fn reads_clojure_types_without_collapsing_them() {
        use Value as V;
        let kw = |s: &str| V::Keyword(s.into());
        // Each fixture is babashka's transit writer output for the form in
        // the comment.
        // [:a :ns/b]
        assert_eq!(
            read(r#"["~:a","~:ns/b"]"#).unwrap(),
            V::Vector(vec![kw("a"), kw("ns/b")])
        );
        // [f \x \newline]
        assert_eq!(
            read(r#"["~$f","~cx","~c\n"]"#).unwrap(),
            V::Vector(vec![V::Symbol("f".into()), V::Char('x'), V::Char('\n')])
        );
        // [1N 12345678901234567890N 1.5M 1/3]
        assert_eq!(
            read(r#"["~n1","~n12345678901234567890","~f1.5",["~#ratio",["~n1","~n3"]]]"#).unwrap(),
            V::Vector(vec![
                V::BigInt("1".into()),
                V::BigInt("12345678901234567890".into()),
                V::BigDec("1.5".into()),
                V::Ratio {
                    numerator: "1".into(),
                    denominator: "3".into()
                },
            ])
        );
        // [(1 2) #{1} {:a 1, "a" 2} {1 2} {[1] 2}]
        assert_eq!(
            read(r#"[["~#list",[1,2]],["~#set",[1]],["^ ","~:a",1,"a",2],["^ ","~i1",2],["~#cmap",[[1],2]]]"#).unwrap(),
            V::Vector(vec![
                V::List(vec![V::Int(1), V::Int(2)]),
                V::Set(vec![V::Int(1)]),
                V::Map(vec![(kw("a"), V::Int(1)), (V::String("a".into()), V::Int(2))]),
                V::Map(vec![(V::Int(1), V::Int(2))]),
                V::Map(vec![(V::Vector(vec![V::Int(1)]), V::Int(2))]),
            ])
        );
        // [(java.net.URI. "http://x") #uuid "550e..." #inst "2026-01-01"]
        assert_eq!(
            read(r#"["~rhttp://x","~u550e8400-e29b-41d4-a716-446655440000","~m1767225600000"]"#)
                .unwrap(),
            V::Vector(vec![
                V::Uri("http://x".into()),
                V::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
                V::Inst(1767225600000),
            ])
        );
        // [##Inf ##-Inf 9007199254740993 1.0 (byte-array [1 2])]
        assert_eq!(
            read(r#"["~zINF","~z-INF","~i9007199254740993",1.0,"~bAQI="]"#).unwrap(),
            V::Vector(vec![
                V::Float(f64::INFINITY),
                V::Float(f64::NEG_INFINITY),
                V::Int(9007199254740993),
                V::Float(1.0),
                V::Bytes(vec![1, 2]),
            ])
        );
        assert!(matches!(read(r#""~zNaN""#).unwrap(), V::Float(f) if f.is_nan()));
        assert_eq!(read("null").unwrap(), V::Nil);
    }

    #[test]
    fn reads_cache_references_and_with_meta_as_values() {
        let text =
            r#"[["^ ","~:name","a"],["^ ","^0","b"],["~#with-meta",[[1],["^ ","~:tag","~:x"]]]]"#;
        let name = Value::Keyword("name".into());
        let v = read(text).unwrap();
        let Value::Vector(items) = &v else {
            panic!("{v:?}")
        };
        assert_eq!(items[1].get(&name), Some(&Value::String("b".into())));
        assert_eq!(
            items[2],
            Value::WithMeta(
                Box::new(Value::Vector(vec![Value::Int(1)])),
                Box::new(Value::Map(vec![(
                    Value::Keyword("tag".into()),
                    Value::Keyword("x".into())
                )]))
            )
        );
        assert!(read(r#""~cxy""#).is_err(), "a char is one character");
        assert!(read(r#"["~#ratio",["~n1",1.5]]"#).is_err());
    }

    #[test]
    fn writes_values_that_read_back_unchanged() {
        use Value as V;
        let v = V::Vector(vec![
            V::Keyword("a".into()),
            V::Symbol("f".into()),
            V::Char('x'),
            V::BigInt("12345678901234567890".into()),
            V::BigDec("1.5".into()),
            V::Ratio {
                numerator: "1".into(),
                denominator: "3".into(),
            },
            V::List(vec![V::Int(1)]),
            V::Set(vec![V::Int(1)]),
            V::Map(vec![
                (V::Keyword("a".into()), V::Int(1)),
                (V::String("a".into()), V::Int(2)),
            ]),
            V::Map(vec![(V::Int(1), V::Int(2))]),
            V::String("~escaped".into()),
            V::Int(9007199254740993),
            V::Float(f64::INFINITY),
            V::Inst(5),
            V::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
            V::Uri("http://x".into()),
            V::Bytes(vec![0, 255]),
            V::Tagged(
                "point".into(),
                Box::new(V::Vector(vec![V::Int(1), V::Int(2)])),
            ),
            V::WithMeta(Box::new(V::Vector(vec![])), Box::new(V::Map(vec![]))),
            V::Nil,
        ]);
        let text = encode(&serde_json::to_value(&v).unwrap());
        assert_eq!(read(&text).unwrap(), v);
        assert!(text.contains(r#"["~#ratio",["~n1","~n3"]]"#), "{text}");
        assert!(text.contains(r#"["~#cmap",[1,2]]"#), "{text}");
    }

    #[test]
    fn values_inside_serde_types_and_under_json() {
        let v = serde_json::to_value(json!({
            "k": Value::Keyword("a".into()),
            "s": Value::Set(vec![Value::Char('x')]),
        }))
        .unwrap();
        assert_eq!(encode(&v), r#"["^ ","~:k","~:a","~:s",["~#set",["~cx"]]]"#);
        assert_eq!(to_plain_json(&v), json!({"k": "a", "s": ["x"]}));
        let v = serde_json::to_value(Value::Vector(vec![
            Value::Bytes(vec![0, 255, 16]),
            Value::Tagged("point".into(), Box::new(Value::Int(1))),
            Value::WithMeta(Box::new(Value::Int(3)), Box::new(Value::Map(vec![]))),
            Value::Ratio {
                numerator: "1".into(),
                denominator: "4".into(),
            },
            Value::Float(f64::NAN),
            Value::Map(vec![(Value::Int(1), Value::Keyword("one".into()))]),
            // Not valid as transit a reader would accept, but still written.
            Value::Tagged("set".into(), Box::new(Value::Int(1))),
        ]))
        .unwrap();
        assert_eq!(
            to_plain_json(&v),
            json!(["AP8Q", {"tag": "point", "rep": 1}, 3, 0.25, "NaN", {"1": "one"}, {"tag": "set", "rep": 1}])
        );
        assert_eq!(
            Value::from(&json!({"a": [1, "x", null, 1.5]})),
            Value::Map(vec![(
                Value::String("a".into()),
                Value::Vector(vec![
                    Value::Int(1),
                    Value::String("x".into()),
                    Value::Nil,
                    Value::Float(1.5)
                ])
            )])
        );
    }

    #[test]
    fn plain_json_drops_markers() {
        let v = serde_json::to_value((
            Keyword::new("a"),
            Set(vec![1, 2]),
            Inst(5),
            Tagged {
                tag: "t".into(),
                rep: json!(1),
            },
            WithMeta {
                value: 3,
                meta: Some(json!({})),
            },
        ))
        .unwrap();
        assert_eq!(
            to_plain_json(&v),
            json!(["a", [1, 2], 5, {"tag": "t", "rep": 1}, 3])
        );
    }
}
