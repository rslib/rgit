//! An ordered value tree and its TOON encoding (https://toonformat.dev), for
//! the agent output modes. Keys keep insertion order, so a schema prints in the
//! order it is declared. A list of plain values is laid out one item per
//! line, the AXI convention.

use serde::ser::{SerializeMap, SerializeSeq};

pub type Obj = Vec<(String, Node)>;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Node>),
    Obj(Obj),
}

/// Build an ordered object: `obj!{ "id" => x, "name" => y }`.
#[macro_export]
macro_rules! obj {
    ($($key:literal => $value:expr),* $(,)?) => {
        vec![$(($key.to_owned(), $crate::toon::Node::from(&($value)))),*]
    };
}

impl Node {
    fn is_primitive(&self) -> bool {
        !matches!(self, Node::List(_) | Node::Obj(_))
    }

    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Node> {
        match self {
            Node::Obj(fields) => fields.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at `key` in an object, inserting `default` at the end when
    /// absent. `None` when `self` is not an object.
    pub fn entry(&mut self, key: &str, default: Node) -> Option<&mut Node> {
        let Node::Obj(fields) = self else {
            return None;
        };
        let i = match fields.iter().position(|(k, _)| k == key) {
            Some(i) => i,
            None => {
                fields.push((key.to_owned(), default));
                fields.len() - 1
            }
        };
        Some(&mut fields[i].1)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&Vec<Node>> {
        match self {
            Node::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Node>> {
        match self {
            Node::List(items) => Some(items),
            _ => None,
        }
    }
}

impl<'de> serde::Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> serde::de::Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_none<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_bool<E>(self, b: bool) -> Result<Node, E> {
        Ok(Node::Bool(b))
    }

    fn visit_i64<E>(self, i: i64) -> Result<Node, E> {
        Ok(Node::Int(i))
    }

    fn visit_u64<E>(self, u: u64) -> Result<Node, E> {
        Ok(i64::try_from(u).map_or(Node::Float(u as f64), Node::Int))
    }

    fn visit_f64<E>(self, f: f64) -> Result<Node, E> {
        Ok(Node::Float(f))
    }

    fn visit_str<E>(self, s: &str) -> Result<Node, E> {
        Ok(Node::Str(s.to_owned()))
    }

    fn visit_string<E>(self, s: String) -> Result<Node, E> {
        Ok(Node::Str(s))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Node::List(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut fields = Obj::new();
        while let Some((k, v)) = map.next_entry::<String, Node>()? {
            fields.push((k, v));
        }
        Ok(Node::Obj(fields))
    }
}

impl From<serde_json::Value> for Node {
    fn from(v: serde_json::Value) -> Self {
        use serde_json::Value;
        match v {
            Value::Null => Node::Null,
            Value::Bool(b) => Node::Bool(b),
            Value::Number(n) => match n.as_i64() {
                Some(i) => Node::Int(i),
                None => Node::Float(n.as_f64().unwrap_or(0.0)),
            },
            Value::String(s) => Node::Str(s),
            Value::Array(items) => Node::List(items.into_iter().map(Node::from).collect()),
            Value::Object(map) => Node::Obj(map.into_iter().map(|(k, v)| (k, v.into())).collect()),
        }
    }
}

impl From<&str> for Node {
    fn from(s: &str) -> Self {
        Node::Str(s.to_owned())
    }
}

impl From<String> for Node {
    fn from(s: String) -> Self {
        Node::Str(s)
    }
}

impl From<bool> for Node {
    fn from(b: bool) -> Self {
        Node::Bool(b)
    }
}

impl From<usize> for Node {
    fn from(n: usize) -> Self {
        Node::Int(n as i64)
    }
}

impl From<u32> for Node {
    fn from(n: u32) -> Self {
        Node::Int(n.into())
    }
}

impl From<f32> for Node {
    fn from(f: f32) -> Self {
        Node::Float(f.into())
    }
}

impl<T: Clone + Into<Node>> From<&T> for Node {
    fn from(v: &T) -> Self {
        v.clone().into()
    }
}

impl From<f64> for Node {
    fn from(f: f64) -> Self {
        Node::Float(f)
    }
}

impl<T: Into<Node>> From<Option<T>> for Node {
    fn from(v: Option<T>) -> Self {
        v.map_or(Node::Null, Into::into)
    }
}

impl<T: Into<Node>> From<Vec<T>> for Node {
    fn from(items: Vec<T>) -> Self {
        Node::List(items.into_iter().map(Into::into).collect())
    }
}

impl<T: Into<Node> + Clone> From<&[T]> for Node {
    fn from(items: &[T]) -> Self {
        Node::List(items.iter().cloned().map(Into::into).collect())
    }
}

impl serde::Serialize for Node {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Node::Null => s.serialize_unit(),
            Node::Bool(b) => s.serialize_bool(*b),
            Node::Int(i) => s.serialize_i64(*i),
            Node::Float(f) => s.serialize_f64(*f),
            Node::Str(v) => s.serialize_str(v),
            Node::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Node::Obj(fields) => {
                let mut map = s.serialize_map(Some(fields.len()))?;
                for (k, v) in fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// Encode a root object as TOON with 2-space indentation and `,` delimiter.
pub fn encode(root: &[(String, Node)]) -> String {
    let mut out = Vec::new();
    write_fields(&mut out, root, 0);
    out.join("\n")
}

fn pad(depth: usize) -> String {
    "  ".repeat(depth)
}

fn write_fields(out: &mut Vec<String>, fields: &[(String, Node)], depth: usize) {
    for (k, v) in fields {
        write_field(out, &key(k), v, depth, &pad(depth));
    }
}

/// One `key: value` field. `lead` is the text before the key: indentation, or
/// indentation plus `- ` for the first field of a list-item object.
fn write_field(out: &mut Vec<String>, key: &str, v: &Node, depth: usize, lead: &str) {
    match v {
        Node::List(items) => write_array(out, key, items, depth, lead),
        Node::Obj(fields) => {
            out.push(format!("{lead}{key}:"));
            write_fields(out, fields, depth + 1);
        }
        prim => out.push(format!("{lead}{key}: {}", primitive(prim))),
    }
}

fn write_array(out: &mut Vec<String>, key: &str, items: &[Node], depth: usize, lead: &str) {
    if items.is_empty() {
        out.push(format!("{lead}{key}: []"));
        return;
    }
    let n = items.len();
    if items.iter().all(Node::is_primitive) {
        out.push(format!("{lead}{key}[{n}]:"));
        for item in items {
            out.push(format!("{}{}", pad(depth + 1), list_value(item)));
        }
        return;
    }
    if let Some(columns) = tabular_columns(items) {
        let header: Vec<String> = columns.iter().map(|c| self::key(c)).collect();
        out.push(format!("{lead}{key}[{n}]{{{}}}:", header.join(",")));
        for item in items {
            let Node::Obj(fields) = item else { continue };
            let cells: Vec<String> = columns
                .iter()
                .map(|c| {
                    fields
                        .iter()
                        .find(|(k, _)| k == c)
                        .map_or_else(|| "null".to_owned(), |(_, v)| primitive(v))
                })
                .collect();
            out.push(format!("{}{}", pad(depth + 1), cells.join(",")));
        }
        return;
    }
    out.push(format!("{lead}{key}[{n}]:"));
    for item in items {
        write_list_item(out, item, depth + 1);
    }
}

/// The shared column order when every item is a non-empty object with the same
/// keys and only primitive values; `None` otherwise.
fn tabular_columns(items: &[Node]) -> Option<Vec<String>> {
    let Node::Obj(first) = &items[0] else {
        return None;
    };
    if first.is_empty() {
        return None;
    }
    let columns: Vec<String> = first.iter().map(|(k, _)| k.clone()).collect();
    let uniform = items.iter().all(|item| match item {
        Node::Obj(fields) => {
            fields.len() == columns.len()
                && fields
                    .iter()
                    .all(|(k, v)| v.is_primitive() && columns.contains(k))
        }
        _ => false,
    });
    uniform.then_some(columns)
}

fn write_list_item(out: &mut Vec<String>, item: &Node, depth: usize) {
    let indent = pad(depth);
    match item {
        Node::Obj(fields) if fields.is_empty() => out.push(format!("{indent}-")),
        Node::Obj(fields) => {
            let (first_key, first_value) = &fields[0];
            write_field(
                out,
                &key(first_key),
                first_value,
                depth + 1,
                &format!("{indent}- "),
            );
            write_fields(out, &fields[1..], depth + 1);
        }
        Node::List(inner) if inner.iter().all(Node::is_primitive) => {
            let cells: Vec<String> = inner.iter().map(primitive).collect();
            let values = if cells.is_empty() {
                String::new()
            } else {
                format!(" {}", cells.join(","))
            };
            out.push(format!("{indent}- [{}]:{values}", inner.len()));
        }
        Node::List(inner) => {
            out.push(format!("{indent}- [{}]:", inner.len()));
            for nested in inner {
                write_list_item(out, nested, depth + 1);
            }
        }
        prim => out.push(format!("{indent}- {}", primitive(prim))),
    }
}

fn primitive(v: &Node) -> String {
    match v {
        Node::Null => "null".to_owned(),
        Node::Bool(b) => b.to_string(),
        Node::Int(i) => i.to_string(),
        Node::Float(f) => number(*f),
        Node::Str(s) if needs_quotes(s) => quote(s),
        Node::Str(s) => s.clone(),
        Node::List(_) | Node::Obj(_) => unreachable!("not a primitive"),
    }
}

fn number(f: f64) -> String {
    if !f.is_finite() {
        return "null".to_owned();
    }
    if f == 0.0 {
        return "0".to_owned();
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    format!("{f}")
}

fn key(k: &str) -> String {
    let mut chars = k.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if valid { k.to_owned() } else { quote(k) }
}

fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.starts_with(char::is_whitespace)
        || s.ends_with(char::is_whitespace)
        || matches!(s, "true" | "false" | "null")
        || numeric_like(s)
        || s.starts_with('-')
        || s.starts_with('#')
        || s.chars()
            .any(|c| matches!(c, ':' | '"' | '\\' | '[' | ']' | '{' | '}' | ',') || c.is_control())
}

/// A plain list item on its own line: unquoted unless quotes keep it intact.
fn list_value(v: &Node) -> String {
    match v {
        Node::Str(s)
            if s.trim().is_empty()
                || s.ends_with(char::is_whitespace)
                || s.starts_with("- ")
                || s.chars().any(char::is_control) =>
        {
            quote(s)
        }
        Node::Str(s) => s.clone(),
        other => primitive(other),
    }
}

fn numeric_like(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    digits.starts_with(|c: char| c.is_ascii_digit())
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_insertion_order_and_quotes_per_spec() {
        let root = obj! {
            "zeta" => "plain text",
            "alpha" => "a-b",
            "lead" => "-x",
            "num" => "42",
            "empty" => Vec::<Node>::new(),
            "tags" => vec!["a", "b,c"],
        };
        assert_eq!(
            encode(&root),
            "zeta: plain text\nalpha: a-b\nlead: \"-x\"\nnum: \"42\"\nempty: []\ntags[2]:\n  a\n  b,c"
        );
    }

    #[test]
    fn encodes_tables_and_lists() {
        let rows = vec![
            Node::Obj(obj! { "id" => "b1", "n" => 2usize }),
            Node::Obj(obj! { "id" => "a2", "n" => 1usize }),
        ];
        let mixed = vec![
            Node::from("x"),
            Node::Obj(obj! { "k" => "v", "m" => 1usize }),
        ];
        let root = obj! { "rows" => rows, "mixed" => mixed };
        assert_eq!(
            encode(&root),
            "rows[2]{id,n}:\n  b1,2\n  a2,1\nmixed[2]:\n  - x\n  - k: v\n    m: 1"
        );
    }

    #[test]
    fn json_round_trip_keeps_key_order() {
        let text = r#"{"zeta":1,"alpha":{"b":true,"a":null},"list":[2.5,"x"]}"#;
        let node: Node = serde_json::from_str(text).unwrap();
        assert_eq!(serde_json::to_string(&node).unwrap(), text);
    }

    #[test]
    fn help_is_one_hint_per_line() {
        let one = obj! { "help" => vec!["Run `x --full` to see it"] };
        assert_eq!(encode(&one), "help[1]:\n  Run `x --full` to see it");
        let two = obj! { "help" => vec!["Run `a`, then `b`", "Run `c`"] };
        assert_eq!(encode(&two), "help[2]:\n  Run `a`, then `b`\n  Run `c`");
    }

    #[test]
    fn plain_lists_keep_leading_space_and_quote_only_to_stay_intact() {
        let root = obj! { "lines" => vec!["[main b1a7ca6] fix", " 5 files changed", "", "x ", "- y", "a\nb"] };
        assert_eq!(
            encode(&root),
            "lines[6]:\n  [main b1a7ca6] fix\n   5 files changed\n  \"\"\n  \"x \"\n  \"- y\"\n  \"a\\nb\""
        );
    }

    #[test]
    fn escapes_controls() {
        let root = obj! { "s" => "a\nb\u{1}" };
        assert_eq!(encode(&root), "s: \"a\\nb\\u0001\"");
    }
}
