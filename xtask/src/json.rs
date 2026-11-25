//! A small JSON parser for the files that xtask reads, such as `tasks.json` (ADR-0011).
//!
//! It is iterative, with an explicit stack bounded by `DEPTH_MAX`, because the no-recursion rule
//! (ADR-0006) covers xtask too. Objects keep their keys in order and reject duplicate keys.
//! Numbers are kept as their source text; nothing here needs arithmetic on them.

/// Deepest nesting of arrays and objects accepted.
const DEPTH_MAX: usize = 64;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, as written in the source.
    Number(String),
    /// A string, with escapes decoded.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object, with its keys in source order.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(text) = self { Some(text) } else { None }
    }

    /// The boolean, if this is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(value) = self { Some(*value) } else { None }
    }

    /// The elements, if this is an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        if let Self::Array(items) = self { Some(items) } else { None }
    }

    /// The members, if this is an object.
    #[must_use]
    pub fn as_object(&self) -> Option<&[(String, Self)]> {
        if let Self::Object(members) = self { Some(members) } else { None }
    }

    /// The value stored under `key`, if this is an object that has it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        let members = self.as_object()?;
        members.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    /// The JSON type name, for error messages.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "a boolean",
            Self::Number(_) => "a number",
            Self::String(_) => "a string",
            Self::Array(_) => "an array",
            Self::Object(_) => "an object",
        }
    }
}

/// A syntax error, with the one-based line where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// One-based line number.
    pub line: u32,
    /// What is wrong.
    pub message: String,
}

/// Parses one JSON document.
///
/// # Errors
///
/// Returns the first syntax error, with its line.
pub fn parse(text: &str) -> Result<Value, Error> {
    let mut parser = Parser { bytes: text.as_bytes(), position: 0 };
    parser.document().map_err(|message| Error { line: parser.line(), message })
}

/// `text` as a JSON string literal, quotes included: for the request bodies that xtask sends.
#[must_use]
pub fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            control if control < ' ' => {
                let code = u32::from(control);
                quoted.push_str("\\u00");
                for digit in [code / 16, code % 16] {
                    quoted.push(char::from_digit(digit, 16).expect("a nibble is a hex digit"));
                }
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    assert!(quoted.len() >= text.len() + 2, "quoting only adds characters");
    quoted
}

/// An array or object whose elements are still being read.
enum Frame {
    Array(Vec<Value>),
    Object { members: Vec<(String, Value)>, key: Option<String> },
}

/// What the parser expects next.
enum State {
    /// The start of any value.
    Value,
    /// Just after `[`: an element or `]`.
    ArrayStart,
    /// Just after `{`: a key or `}`.
    ObjectStart,
    /// A key and its colon.
    Key,
    /// After a complete element: `,` or the closing bracket.
    AfterValue,
    /// A value has been completed and must be stored in its container.
    Complete(Value),
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn document(&mut self) -> Result<Value, String> {
        let mut stack: Vec<Frame> = Vec::new();
        let mut state = State::Value;
        // Every step either consumes input or completes a container, so this bound is never hit
        // by valid input; it keeps the loop bounded for any input.
        let steps_max = 4 * self.bytes.len() + 8;
        for _ in 0..steps_max {
            state = match state {
                State::Value => self.value_start(&mut stack)?,
                State::ArrayStart => self.close_or(&mut stack, b']', State::Value)?,
                State::ObjectStart => self.close_or(&mut stack, b'}', State::Key)?,
                State::Key => self.key(&mut stack)?,
                State::AfterValue => self.after_value(&mut stack)?,
                State::Complete(value) if stack.is_empty() => return self.end(value),
                State::Complete(value) => {
                    store(&mut stack, value)?;
                    State::AfterValue
                }
            };
        }
        Err("JSON document is too complex".to_owned())
    }

    /// Just after an opening bracket: the matching close, or else `otherwise`.
    fn close_or(
        &mut self,
        stack: &mut Vec<Frame>,
        closing: u8,
        otherwise: State,
    ) -> Result<State, String> {
        self.skip_whitespace();
        if self.eat(closing) { close(stack) } else { Ok(otherwise) }
    }

    /// The top-level value is complete; only whitespace may follow.
    fn end(&mut self, value: Value) -> Result<Value, String> {
        self.skip_whitespace();
        if self.position != self.bytes.len() {
            return Err("unexpected text after the JSON value".to_owned());
        }
        Ok(value)
    }

    fn value_start(&mut self, stack: &mut Vec<Frame>) -> Result<State, String> {
        self.skip_whitespace();
        let byte = self.peek().ok_or("unexpected end of input; expected a value")?;
        if byte == b'[' || byte == b'{' {
            if stack.len() >= DEPTH_MAX {
                return Err(format!("nesting deeper than {DEPTH_MAX}"));
            }
            self.position += 1;
            if byte == b'[' {
                stack.push(Frame::Array(Vec::new()));
                return Ok(State::ArrayStart);
            }
            stack.push(Frame::Object { members: Vec::new(), key: None });
            return Ok(State::ObjectStart);
        }
        let value = match byte {
            b'"' => Value::String(self.string()?),
            b't' => self.literal("true", Value::Bool(true))?,
            b'f' => self.literal("false", Value::Bool(false))?,
            b'n' => self.literal("null", Value::Null)?,
            b'-' | b'0'..=b'9' => Value::Number(self.number()?),
            _ => return Err(format!("unexpected character `{}`", char::from(byte))),
        };
        Ok(State::Complete(value))
    }

    fn key(&mut self, stack: &mut [Frame]) -> Result<State, String> {
        self.skip_whitespace();
        if self.peek() != Some(b'"') {
            return Err("expected a string key".to_owned());
        }
        let name = self.string()?;
        self.skip_whitespace();
        if !self.eat(b':') {
            return Err(format!("expected `:` after the key \"{name}\""));
        }
        let Some(Frame::Object { members, key }) = stack.last_mut() else {
            unreachable!("keys are only read inside objects");
        };
        if members.iter().any(|(existing, _)| *existing == name) {
            return Err(format!("duplicate key \"{name}\""));
        }
        assert!(key.is_none(), "a key is consumed before the next one is read");
        *key = Some(name);
        Ok(State::Value)
    }

    fn after_value(&mut self, stack: &mut Vec<Frame>) -> Result<State, String> {
        self.skip_whitespace();
        let in_array = matches!(stack.last(), Some(Frame::Array(_)));
        let closing = if in_array { b']' } else { b'}' };
        if self.eat(b',') {
            return Ok(if in_array { State::Value } else { State::Key });
        }
        if self.eat(closing) {
            return close(stack);
        }
        Err(format!("expected `,` or `{}`", char::from(closing)))
    }

    fn string(&mut self) -> Result<String, String> {
        assert!(self.eat(b'"'), "strings start with a quote");
        let mut decoded = Vec::new();
        for _ in 0..=self.bytes.len() {
            let byte = self.next().ok_or("unterminated string")?;
            match byte {
                b'"' => {
                    return String::from_utf8(decoded).map_err(|_| "invalid UTF-8".to_owned());
                }
                b'\\' => {
                    let character = self.escape()?;
                    let mut buffer = [0; 4];
                    decoded.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                }
                0x00..=0x1f => return Err("control character in a string".to_owned()),
                _ => decoded.push(byte),
            }
        }
        Err("unterminated string".to_owned())
    }

    fn escape(&mut self) -> Result<char, String> {
        let byte = self.next().ok_or("unterminated escape")?;
        let character = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode_escape(),
            _ => return Err(format!("invalid escape `\\{}`", char::from(byte))),
        };
        Ok(character)
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let high = self.hex4()?;
        if !(0xD800..0xDC00).contains(&high) {
            return char::from_u32(high).ok_or_else(|| "invalid \\u escape".to_owned());
        }
        // A high surrogate must be followed by `\u` and a low surrogate.
        if !(self.eat(b'\\') && self.eat(b'u')) {
            return Err("unpaired surrogate in \\u escape".to_owned());
        }
        let low = self.hex4()?;
        if !(0xDC00..0xE000).contains(&low) {
            return Err("unpaired surrogate in \\u escape".to_owned());
        }
        let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
        char::from_u32(code).ok_or_else(|| "invalid \\u escape".to_owned())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.bytes.get(self.position..self.position + 4).ok_or("short \\u escape")?;
        let text = std::str::from_utf8(digits).map_err(|_| "invalid \\u escape".to_owned())?;
        let value = u32::from_str_radix(text, 16).map_err(|_| "invalid \\u escape".to_owned())?;
        self.position += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<String, String> {
        let start = self.position;
        let _negative = self.eat(b'-');
        if self.eat(b'0') {
            // A leading zero stands alone.
        } else if self.digits() == 0 {
            return Err("invalid number".to_owned());
        }
        if self.eat(b'.') && self.digits() == 0 {
            return Err("invalid number: no digits after `.`".to_owned());
        }
        if self.eat(b'e') || self.eat(b'E') {
            let _sign = self.eat(b'+') || self.eat(b'-');
            if self.digits() == 0 {
                return Err("invalid number: no digits in the exponent".to_owned());
            }
        }
        let text = self.bytes.get(start..self.position).expect("the number lies within the input");
        assert!(!text.is_empty(), "a number has at least one character");
        Ok(String::from_utf8_lossy(text).into_owned())
    }

    fn digits(&mut self) -> u32 {
        let mut count = 0;
        for _ in 0..=self.bytes.len() {
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                break;
            }
            self.position += 1;
            count += 1;
        }
        count
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        let end = self.position + word.len();
        if self.bytes.get(self.position..end) != Some(word.as_bytes()) {
            return Err(format!("invalid literal; expected `{word}`"));
        }
        self.position = end;
        Ok(value)
    }

    fn skip_whitespace(&mut self) {
        for _ in 0..=self.bytes.len() {
            if !self.peek().is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r')) {
                return;
            }
            self.position += 1;
        }
    }

    fn eat(&mut self, expected: u8) -> bool {
        let found = self.peek() == Some(expected);
        if found {
            self.position += 1;
        }
        found
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn line(&self) -> u32 {
        let consumed = self.bytes.get(..self.position).unwrap_or(self.bytes);
        let lines = consumed.split(|&byte| byte == b'\n').count();
        u32::try_from(lines).unwrap_or(u32::MAX)
    }
}

/// Pops the innermost container, which has just been closed.
fn close(stack: &mut Vec<Frame>) -> Result<State, String> {
    let value = match stack.pop() {
        Some(Frame::Array(items)) => Value::Array(items),
        Some(Frame::Object { members, key: None }) => Value::Object(members),
        Some(Frame::Object { key: Some(key), .. }) => {
            return Err(format!("the key \"{key}\" has no value"));
        }
        None => unreachable!("close is only called inside a container"),
    };
    Ok(State::Complete(value))
}

/// Stores a completed value in the innermost container.
fn store(stack: &mut [Frame], value: Value) -> Result<(), String> {
    match stack.last_mut() {
        Some(Frame::Array(items)) => items.push(value),
        Some(Frame::Object { members, key }) => {
            let name = key.take().ok_or("an object member is missing its key")?;
            members.push((name, value));
        }
        None => unreachable!("store is only called inside a container"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Value, parse, quote};

    fn string(text: &str) -> Value {
        Value::String(text.to_owned())
    }

    // Goal: a quoted string parses back to itself, and each escape is the short form where JSON
    // has one. Method: every character class that needs an escape, plus text that does not.
    #[test]
    fn quoted_strings_parse_back() {
        assert_eq!(quote("plain é"), "\"plain é\"");
        assert_eq!(quote("a\"b\\c\nd\re\tf"), r#""a\"b\\c\nd\re\tf""#);
        assert_eq!(quote("\u{0}\u{1f}"), r#""\u0000\u001f""#);
        for text in ["", "# Title\n\n- Found in: #5\n", "\u{7f} \u{1F600} \u{1b}[0m", "\\\"\\"] {
            assert_eq!(parse(&quote(text)), Ok(string(text)), "{text:?}");
        }
    }

    // Goal: every JSON construct parses to the expected value, nested containers included.
    #[test]
    fn documents_parse() {
        let text = r#" {"a": [1, -2.5e3, true, false, null], "b": {"c": "d"}, "e": [], "f": {}} "#;
        let value = parse(text).expect("valid JSON parses");
        let expected = Value::Object(vec![
            (
                "a".to_owned(),
                Value::Array(vec![
                    Value::Number("1".to_owned()),
                    Value::Number("-2.5e3".to_owned()),
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Null,
                ]),
            ),
            ("b".to_owned(), Value::Object(vec![("c".to_owned(), string("d"))])),
            ("e".to_owned(), Value::Array(Vec::new())),
            ("f".to_owned(), Value::Object(Vec::new())),
        ]);
        assert_eq!(value, expected);
        assert_eq!(parse("0").expect("a bare number parses"), Value::Number("0".to_owned()));
        assert_eq!(parse("1E5").expect("capital exponent"), Value::Number("1E5".to_owned()));
        assert_eq!(parse("1e-5").expect("negative exponent"), Value::Number("1e-5".to_owned()));
        assert_eq!(parse("1e+5").expect("positive exponent"), Value::Number("1e+5".to_owned()));
        assert_eq!(parse("[[[]]]").expect("nested arrays parse").kind(), "an array");
    }

    // Goal: string escapes decode, including a surrogate pair.
    #[test]
    fn escapes_decode() {
        let value = parse(r#""a\"b\\c\/d\n\t\u00e9\ud83d\ude00\b\f\r""#).expect("escapes parse");
        assert_eq!(value, string("a\"b\\c/d\n\té\u{1F600}\u{8}\u{c}\r"));
    }

    // Goal: invalid documents fail with a message and the line of the error.
    #[test]
    fn invalid_documents_fail() {
        let cases = [
            ("", "unexpected end"),
            ("[1,]", "unexpected character"),
            ("{\"a\" 1}", "expected `:`"),
            ("{\"a\": 1, \"a\": 2}", "duplicate key"),
            ("[1 2]", "expected `,` or `]`"),
            ("{\"a\": 1 \"b\"}", "expected `,` or `}`"),
            ("{1: 2}", "expected a string key"),
            ("\"abc", "unterminated string"),
            ("\"a\u{1}\"", "control character"),
            ("\"\\x\"", "invalid escape"),
            ("\"\\ud83d\"", "unpaired surrogate"),
            ("\"\\ud83d\\u0041\"", "unpaired surrogate"),
            ("\"\\u12\"", "short \\u escape"),
            ("\"\\u12zz\"", "invalid \\u escape"),
            ("01", "unexpected text"),
            ("-", "invalid number"),
            ("1.", "no digits after"),
            ("1e+", "no digits in the exponent"),
            ("tru", "expected `true`"),
            ("[] []", "unexpected text"),
        ];
        for (text, expected) in cases {
            let error = parse(text).expect_err("invalid JSON fails");
            assert!(error.message.contains(expected), "{text:?}: {}", error.message);
        }
        assert_eq!(parse("[\n1,\n,]").expect_err("error on line 3").line, 3);
        let deep = format!("{}{}", "[".repeat(65), "]".repeat(65));
        assert!(parse(&deep).expect_err("too deep").message.contains("nesting"), "depth bound");
        assert!(parse(&"[".repeat(64)).is_err(), "unterminated nesting fails");
        let deepest = format!("{}{}", "[".repeat(64), "]".repeat(64));
        assert!(
            parse(&deepest).is_ok(),
            "the deepest allowed nesting parses within the step bound"
        );
    }

    // Goal: the accessors return the content only for the matching type.
    #[test]
    fn accessors_match_types() {
        let value = parse(r#"{"s": "x", "b": true, "a": [1], "o": {}}"#).expect("valid JSON");
        assert_eq!(value.get("s").and_then(Value::as_str), Some("x"));
        assert_eq!(value.get("b").and_then(Value::as_bool), Some(true));
        assert_eq!(value.get("a").and_then(Value::as_array).map(<[Value]>::len), Some(1));
        assert_eq!(value.get("o").and_then(Value::as_object).map(<[_]>::len), Some(0));
        assert_eq!(value.get("s").and_then(Value::as_bool), None);
        assert_eq!(value.get("b").and_then(Value::as_str), None);
        assert_eq!(value.get("s").and_then(Value::as_array), None);
        assert_eq!(value.get("s").and_then(Value::as_object), None);
        assert_eq!(value.get("missing"), None);
        assert_eq!(string("x").get("s"), None);
        let kinds: Vec<&str> = ["null", "true", "1", "\"x\"", "[]", "{}"]
            .iter()
            .map(|t| parse(t).unwrap().kind())
            .collect();
        assert_eq!(kinds, ["null", "a boolean", "a number", "a string", "an array", "an object"]);
    }
}
