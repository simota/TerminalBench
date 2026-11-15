//! `tbench validate` — Reporting Spec v1 reference validator:
//! conformance, pairwise comparability, profiles.
//!
//! Ported from a Python reference that is no longer in the repository; the
//! conformance suite (tests/report-v1.sh) is what pins its behaviour now.
//! It uses a hand-rolled JSON reader so
//! duplicate keys, NaN/Infinity, and the int/float distinction match Python's
//! `json` module (serde_json silently keeps the last duplicate and coerces
//! integers), and it reproduces Python's `json.dumps` formatting (sorted keys,
//! `ensure_ascii`, float `repr`) byte-for-byte for the emitted diagnostics.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_IO: i32 = 3;
const EXIT_CONFORMANCE: i32 = 4;
const EXIT_WIRE: i32 = 5;
const EXIT_PROFILE: i32 = 6;
const EXIT_NOT_COMPARABLE: i32 = 7;
const EXIT_UNDETERMINED: i32 = 8;

const SCHEMA_ID: &str = "urn:tbench:schema:report:v1";
const PROTOCOL_ID: &str = "urn:tbench:protocol:reporting";
const MAX_NUMBER: f64 = 1e308;
const REFRESH_TOLERANCE_HZ: f64 = 0.1;

const SYNC_STATUSES: [&str; 3] = ["supported", "unsupported", "no_reply"];
const POWER_SOURCES: [&str; 3] = ["ac", "battery", "ups"];

// ---------------------------------------------------------------------------
// JSON value model + hand-rolled reader
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Value {
    Null,
    Bool(bool),
    Int(i128),
    BigInt { neg: bool, digits: String },
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

#[derive(Debug)]
enum ParseErr {
    Duplicate(String),
    NonFinite,
    Syntax,
}

impl Value {
    fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) | Value::BigInt { .. } => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::Array(_) => "list",
            Value::Object(_) => "dict",
        }
    }

    fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Object member lookup (first match; the reader rejects duplicates).
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Like Python `dict.get(key)`: missing member reads as `None`.
    fn getn(&self, key: &str) -> Value {
        self.get(key).cloned().unwrap_or(Value::Null)
    }

    fn keys(&self) -> Vec<&str> {
        match self {
            Value::Object(pairs) => pairs.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

/// Numeric value as f64 for numbers only (Python bool is *not* a number here).
fn numeric_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::BigInt { neg, digits } => {
            let mag: f64 = digits.parse().unwrap_or(f64::INFINITY);
            Some(if *neg { -mag } else { mag })
        }
        _ => None,
    }
}

fn num_or_bool(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => numeric_f64(v),
    }
}

fn is_number(v: &Value) -> bool {
    match numeric_f64(v) {
        Some(x) => x.is_finite() && x.abs() <= MAX_NUMBER,
        None => false,
    }
}

fn is_int(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::BigInt { .. })
}

/// Python value equality (order-independent objects, `True == 1`).
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            if x.len() != y.len() {
                return false;
            }
            for (k, v) in x {
                match b.get(k) {
                    Some(w) if json_eq(v, w) => {}
                    _ => return false,
                }
            }
            true
        }
        _ => match (num_or_bool(a), num_or_bool(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn peek(&self) -> u8 {
        if self.i < self.b.len() {
            self.b[self.i]
        } else {
            0
        }
    }

    fn skip_ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Value, ParseErr> {
        self.skip_ws();
        match self.peek() {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Value::Str(self.string()?)),
            b't' => self.literal("true", Value::Bool(true)),
            b'f' => self.literal("false", Value::Bool(false)),
            b'n' => self.literal("null", Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            b'N' | b'I' => Err(ParseErr::NonFinite), // NaN / Infinity
            _ => Err(ParseErr::Syntax),
        }
    }

    fn literal(&mut self, word: &str, val: Value) -> Result<Value, ParseErr> {
        let w = word.as_bytes();
        if self.i + w.len() <= self.b.len() && &self.b[self.i..self.i + w.len()] == w {
            self.i += w.len();
            Ok(val)
        } else {
            Err(ParseErr::Syntax)
        }
    }

    fn object(&mut self) -> Result<Value, ParseErr> {
        self.i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        self.skip_ws();
        if self.peek() == b'}' {
            self.i += 1;
            return Ok(Value::Object(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != b'"' {
                return Err(ParseErr::Syntax);
            }
            let key = self.string()?;
            if pairs.iter().any(|(k, _)| k == &key) {
                return Err(ParseErr::Duplicate(key));
            }
            self.skip_ws();
            if self.peek() != b':' {
                return Err(ParseErr::Syntax);
            }
            self.i += 1;
            let val = self.value()?;
            pairs.push((key, val));
            self.skip_ws();
            match self.peek() {
                b',' => {
                    self.i += 1;
                    continue;
                }
                b'}' => {
                    self.i += 1;
                    break;
                }
                _ => return Err(ParseErr::Syntax),
            }
        }
        Ok(Value::Object(pairs))
    }

    fn array(&mut self) -> Result<Value, ParseErr> {
        self.i += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == b']' {
            self.i += 1;
            return Ok(Value::Array(items));
        }
        loop {
            let val = self.value()?;
            items.push(val);
            self.skip_ws();
            match self.peek() {
                b',' => {
                    self.i += 1;
                    continue;
                }
                b']' => {
                    self.i += 1;
                    break;
                }
                _ => return Err(ParseErr::Syntax),
            }
        }
        Ok(Value::Array(items))
    }

    fn read_hex4(&mut self) -> Result<u32, ParseErr> {
        if self.i + 4 > self.b.len() {
            return Err(ParseErr::Syntax);
        }
        let mut v: u32 = 0;
        for _ in 0..4 {
            let c = self.b[self.i];
            let d = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a' + 10) as u32,
                b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return Err(ParseErr::Syntax),
            };
            v = v * 16 + d;
            self.i += 1;
        }
        Ok(v)
    }

    fn string(&mut self) -> Result<String, ParseErr> {
        self.i += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            if self.i >= self.b.len() {
                return Err(ParseErr::Syntax);
            }
            let c = self.b[self.i];
            if c == b'"' {
                self.i += 1;
                break;
            } else if c == b'\\' {
                self.i += 1;
                if self.i >= self.b.len() {
                    return Err(ParseErr::Syntax);
                }
                let e = self.b[self.i];
                self.i += 1;
                match e {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        let cp = self.read_hex4()?;
                        let ch = if (0xD800..=0xDBFF).contains(&cp) {
                            // high surrogate: try to pair with a following \uXXXX
                            if self.i + 2 <= self.b.len()
                                && self.b[self.i] == b'\\'
                                && self.b[self.i + 1] == b'u'
                            {
                                self.i += 2;
                                let lo = self.read_hex4()?;
                                if (0xDC00..=0xDFFF).contains(&lo) {
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    char::from_u32(c).unwrap_or('\u{FFFD}')
                                } else {
                                    '\u{FFFD}'
                                }
                            } else {
                                '\u{FFFD}'
                            }
                        } else {
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        };
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    _ => return Err(ParseErr::Syntax),
                }
            } else if c < 0x20 {
                return Err(ParseErr::Syntax); // raw control char
            } else {
                out.push(c);
                self.i += 1;
            }
        }
        String::from_utf8(out).map_err(|_| ParseErr::Syntax)
    }

    fn number(&mut self) -> Result<Value, ParseErr> {
        let start = self.i;
        let mut is_float = false;
        if self.peek() == b'-' {
            self.i += 1;
        }
        if self.peek() == b'I' {
            return Err(ParseErr::NonFinite); // -Infinity
        }
        if !self.peek().is_ascii_digit() {
            return Err(ParseErr::Syntax);
        }
        if self.peek() == b'0' {
            self.i += 1;
        } else {
            while self.peek().is_ascii_digit() {
                self.i += 1;
            }
        }
        if self.peek() == b'.' {
            is_float = true;
            self.i += 1;
            if !self.peek().is_ascii_digit() {
                return Err(ParseErr::Syntax);
            }
            while self.peek().is_ascii_digit() {
                self.i += 1;
            }
        }
        if matches!(self.peek(), b'e' | b'E') {
            is_float = true;
            self.i += 1;
            if matches!(self.peek(), b'+' | b'-') {
                self.i += 1;
            }
            if !self.peek().is_ascii_digit() {
                return Err(ParseErr::Syntax);
            }
            while self.peek().is_ascii_digit() {
                self.i += 1;
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        if is_float {
            let f: f64 = text.parse().map_err(|_| ParseErr::Syntax)?;
            Ok(Value::Float(f))
        } else {
            match text.parse::<i128>() {
                Ok(n) => Ok(Value::Int(n)),
                Err(_) => Ok(Value::BigInt {
                    neg: text.starts_with('-'),
                    digits: text.trim_start_matches('-').to_string(),
                }),
            }
        }
    }
}

fn parse(text: &str) -> Result<Value, ParseErr> {
    let mut r = Reader {
        b: text.as_bytes(),
        i: 0,
    };
    let v = r.value()?;
    r.skip_ws();
    if r.i != r.b.len() {
        return Err(ParseErr::Syntax);
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Python-compatible serialization
// ---------------------------------------------------------------------------

/// Reproduce CPython `repr(float)` (shortest round-trip, fixed/exponential
/// switch at decpt <= -4 or > 16, `Infinity`/`NaN` like `json.dumps`).
fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let neg = f < 0.0;
    let mag = f.abs();
    let sci = format!("{:e}", mag); // e.g. "1.5e0", "1e2"
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let ndigits = digits.len() as i32;
    let decpt = exp + 1;
    let body = if decpt <= -4 || decpt > 16 {
        let first = &digits[..1];
        let rest = &digits[1..];
        let mant_str = if rest.is_empty() {
            first.to_string()
        } else {
            format!("{}.{}", first, rest)
        };
        let e = decpt - 1;
        let sign = if e < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", mant_str, sign, e.abs())
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if decpt >= ndigits {
        format!("{}{}.0", digits, "0".repeat((decpt - ndigits) as usize))
    } else {
        format!(
            "{}.{}",
            &digits[..decpt as usize],
            &digits[decpt as usize..]
        )
    };
    if neg {
        format!("-{}", body)
    } else {
        body
    }
}

/// JSON string escaping with `ensure_ascii=True` (matches Python's encoder).
fn escape_str(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (0x20..=0x7e).contains(&(c as u32)) => out.push(c),
            c => {
                let u = c as u32;
                if u > 0xFFFF {
                    let v = u - 0x10000;
                    let hi = 0xD800 + (v >> 10);
                    let lo = 0xDC00 + (v & 0x3FF);
                    out.push_str(&format!("\\u{:04x}\\u{:04x}", hi, lo));
                } else {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
        }
    }
    out.push('"');
}

/// Serialize a value like `json.dumps(..., sort_keys=True)`. `spaced` selects
/// `(", ", ": ")` (default) vs the compact `(",", ":")` separators.
fn serialize(v: &Value, spaced: bool, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::BigInt { neg, digits } => {
            if *neg {
                out.push('-');
            }
            out.push_str(digits);
        }
        Value::Float(f) => out.push_str(&py_float(*f)),
        Value::Str(s) => escape_str(s, out),
        Value::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push_str(if spaced { ", " } else { "," });
                }
                serialize(item, spaced, out);
            }
            out.push(']');
        }
        Value::Object(pairs) => {
            let mut sorted: Vec<&(String, Value)> = pairs.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (idx, (k, val)) in sorted.iter().enumerate() {
                if idx > 0 {
                    out.push_str(if spaced { ", " } else { "," });
                }
                escape_str(k, out);
                out.push_str(if spaced { ": " } else { ":" });
                serialize(val, spaced, out);
            }
            out.push('}');
        }
    }
}

fn dumps(v: &Value, spaced: bool) -> String {
    let mut s = String::new();
    serialize(v, spaced, &mut s);
    s
}

/// Python `str(value)` for scalars (used to build compare pointer labels).
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Int(i) => i.to_string(),
        Value::BigInt { neg, digits } => format!("{}{}", if *neg { "-" } else { "" }, digits),
        Value::Float(f) => py_float(*f),
        Value::Str(s) => s.clone(),
        other => dumps(other, false),
    }
}

/// Python `repr(value)` for scalars (used only as a sort key).
fn py_repr(v: &Value) -> String {
    match v {
        Value::Str(s) => {
            let mut out = String::from("'");
            for ch in s.chars() {
                match ch {
                    '\\' => out.push_str("\\\\"),
                    '\'' => out.push_str("\\'"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c => out.push(c),
                }
            }
            out.push('\'');
            out
        }
        other => py_str(other),
    }
}

fn pointer_token(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

// ---------------------------------------------------------------------------
// Profile registry
// ---------------------------------------------------------------------------

pub(crate) struct Profile {
    pub(crate) metric: &'static str,
    pub(crate) unit: &'static str,
    pub(crate) profile_id: &'static str,
    pub(crate) classification: &'static str,
    pub(crate) method_id: &'static str,
    pub(crate) method_version: &'static str,
    pub(crate) scope: &'static str,
    pub(crate) observed_boundary: &'static str,
    pub(crate) direction: &'static str,
    pub(crate) limitation: &'static str,
    pub(crate) excluded: &'static str,
    pub(crate) backends: &'static [&'static str],
    pub(crate) variants: &'static [Option<&'static str>],
    pub(crate) shell_required: bool,
    pub(crate) workload: &'static str,
}

impl Profile {
    /// Value of the named immutable field, matching the Python profile dict.
    fn field(&self, name: &str) -> Value {
        match name {
            "profile_id" => Value::Str(self.profile_id.into()),
            "classification" => Value::Str(self.classification.into()),
            "method_id" => Value::Str(self.method_id.into()),
            "method_version" => Value::Str(self.method_version.into()),
            "scope" => Value::Str(self.scope.into()),
            "observed_boundary" => Value::Str(self.observed_boundary.into()),
            "unit" => Value::Str(self.unit.into()),
            "direction" => Value::Str(self.direction.into()),
            "limitations" => Value::Array(vec![Value::Str(self.limitation.into())]),
            "excluded_claims" => Value::Array(vec![Value::Str(self.excluded.into())]),
            _ => Value::Null,
        }
    }
}

/// Every registered profile. This is the only copy in code: the emitter
/// looks rows up here, and the tests below check that the specification
/// and the JSON Schema carry the same strings.
const PROFILES: &[Profile] = &[
    Profile {
        metric: "shell.startup",
        unit: "ms",
        profile_id: "org.terminalbench.shell.startup",
        classification: "sampled",
        method_id: "org.terminalbench.method.process-wall-clock",
        method_version: "1.0.0",
        scope: "shell-process-startup",
        observed_boundary: "process-invocation-to-process-exit",
        direction: "lower-is-better",
        limitation: "Includes shell startup configuration and process scheduling; it does not isolate terminal rendering.",
        excluded: "Terminal rendering latency",
        backends: &["hyperfine", "zsh-timer", "rust-timer"],
        variants: &[None],
        shell_required: true,
        workload: "urn:tbench:workload:shell-startup:v1",
    },
    Profile {
        metric: "cmd.overhead",
        unit: "us",
        profile_id: "org.terminalbench.command.overhead",
        classification: "sampled",
        method_id: "org.terminalbench.method.process-wall-clock",
        method_version: "1.0.0",
        scope: "shell-command-process",
        observed_boundary: "process-invocation-through-command-exit",
        direction: "lower-is-better",
        limitation: "Includes process spawn and shell initialization in addition to builtin dispatch.",
        excluded: "In-process command dispatch latency",
        backends: &["hyperfine", "zsh-timer", "rust-timer"],
        variants: &[None],
        shell_required: true,
        workload: "urn:tbench:workload:command-overhead:v1",
    },
    Profile {
        metric: "render.throughput",
        unit: "MBps",
        profile_id: "org.terminalbench.render.throughput.bytes",
        classification: "proxy",
        method_id: "org.terminalbench.method.pty-write-wall-clock",
        method_version: "1.0.0",
        scope: "pty-output-pipeline",
        observed_boundary: "fixture-write-start-to-producer-exit",
        direction: "higher-is-better",
        limitation: "Measures producer-side PTY write completion; terminal parsing, final-frame presentation, and frame skipping are not observed.",
        excluded: "On-screen rendering throughput",
        backends: &["zsh-timer", "rust-timer", "rust-writer"],
        variants: &[Some("plain"), Some("ansi"), Some("cjk")],
        shell_required: false,
        workload: "urn:tbench:workload:render-{variant}:v1",
    },
    Profile {
        metric: "render.throughput",
        unit: "linesps",
        profile_id: "org.terminalbench.render.throughput.lines",
        classification: "proxy",
        method_id: "org.terminalbench.method.pty-write-wall-clock",
        method_version: "1.0.0",
        scope: "pty-output-pipeline",
        observed_boundary: "fixture-write-start-to-producer-exit",
        direction: "higher-is-better",
        limitation: "Measures producer-side PTY write completion; terminal parsing, final-frame presentation, and frame skipping are not observed.",
        excluded: "On-screen rendering throughput",
        backends: &["zsh-timer", "rust-timer", "rust-writer"],
        variants: &[Some("plain"), Some("ansi"), Some("cjk")],
        shell_required: false,
        workload: "urn:tbench:workload:render-{variant}:v1",
    },
    Profile {
        metric: "scroll.proxy",
        unit: "ms",
        profile_id: "org.terminalbench.scroll.proxy",
        classification: "proxy",
        method_id: "org.terminalbench.method.pty-write-wall-clock",
        method_version: "1.0.0",
        scope: "pty-output-and-scrollback-proxy",
        observed_boundary: "fixture-write-start-to-producer-exit",
        direction: "lower-is-better",
        limitation: "No scroll, redraw, frame-presentation, or display event is observed.",
        excluded: "On-screen scroll latency",
        backends: &["zsh-timer", "rust-timer", "rust-writer"],
        variants: &[None],
        shell_required: false,
        workload: "urn:tbench:workload:scroll-proxy:v1",
    },
    Profile {
        metric: "input.latency",
        unit: "ms",
        profile_id: "org.terminalbench.input.pty-round-trip",
        classification: "proxy",
        method_id: "org.terminalbench.method.zpty-echo-round-trip",
        method_version: "1.0.0",
        scope: "pseudo-terminal-echo",
        observed_boundary: "byte-write-to-pty-echo-read",
        direction: "lower-is-better",
        limitation: "Uses a synthetic PTY echo and excludes physical input devices, GUI event routing, rendering, and display presentation.",
        excluded: "Physical-keypress-to-photon latency",
        backends: &["zsh-zpty", "rust-pty"],
        variants: &[Some("idle"), Some("loaded")],
        shell_required: false,
        workload: "urn:tbench:workload:input-{variant}:v1",
    },
    Profile {
        metric: "resource.mem",
        unit: "MB",
        profile_id: "org.terminalbench.resource.memory",
        classification: "sampled",
        method_id: "org.terminalbench.method.process-sample",
        method_version: "1.0.0",
        scope: "terminal-process-resident-memory",
        observed_boundary: "identified-terminal-process-rss-sample",
        direction: "lower-is-better",
        limitation: "Samples one identified process and may omit helper-process or shared-memory costs.",
        excluded: "Whole-system memory cost",
        backends: &["ps", "procfs"],
        variants: &[Some("idle"), Some("load")],
        shell_required: false,
        workload: "urn:tbench:workload:resource-{variant}:v1",
    },
    Profile {
        metric: "resource.cpu",
        unit: "pct",
        profile_id: "org.terminalbench.resource.cpu",
        classification: "sampled",
        method_id: "org.terminalbench.method.process-sample",
        method_version: "1.0.0",
        scope: "terminal-process-cpu",
        observed_boundary: "identified-terminal-process-cpu-sample",
        direction: "lower-is-better",
        limitation: "Process CPU is a sampled operating-system statistic and is not an instantaneous whole-system measurement.",
        excluded: "Whole-system CPU cost",
        backends: &["ps"],
        variants: &[Some("idle"), Some("load")],
        shell_required: false,
        workload: "urn:tbench:workload:resource-{variant}:v1",
    },
    Profile {
        metric: "resource.cpu",
        unit: "pct",
        profile_id: "org.terminalbench.resource.cpu.window",
        classification: "sampled",
        method_id: "org.terminalbench.method.process-cpu-time-window",
        method_version: "1.0.0",
        scope: "terminal-process-cpu",
        observed_boundary: "identified-terminal-process-cpu-time-over-wall-clock-window",
        direction: "lower-is-better",
        limitation: "Covers one identified process over fixed wall-clock windows at kernel clock-tick resolution; helper processes are not included.",
        excluded: "Whole-system CPU cost",
        backends: &["procfs"],
        variants: &[Some("idle"), Some("load")],
        shell_required: false,
        workload: "urn:tbench:workload:resource-window-{variant}:v1",
    },
    Profile {
        metric: "fire.fps",
        unit: "fps",
        profile_id: "org.terminalbench.fire.fps.geometry",
        classification: "proxy",
        method_id: "org.terminalbench.method.pty-write-wall-clock",
        method_version: "1.0.0",
        scope: "pty-output-pipeline",
        observed_boundary: "frame-write-start-to-producer-exit",
        direction: "higher-is-better",
        limitation: "Measures producer-side PTY write completion for a truecolor repaint stream at the cell geometry recorded by variant; terminal parsing, compositing, and confirmed on-screen frame presentation are not observed.",
        excluded: "Confirmed on-screen frame rate",
        backends: &["fire-v2-overlapped"],
        variants: &[],
        shell_required: false,
        workload: "urn:tbench:workload:fire-{variant}:v1",
    },
    Profile {
        metric: "fire.fps",
        unit: "fps",
        profile_id: "org.terminalbench.fire.fps",
        classification: "proxy",
        method_id: "org.terminalbench.method.pty-write-wall-clock",
        method_version: "1.0.0",
        scope: "pty-output-pipeline",
        observed_boundary: "frame-write-start-to-producer-exit",
        direction: "higher-is-better",
        limitation: "Measures producer-side PTY write completion for a live window-sized truecolor repaint stream; terminal parsing, compositing, and confirmed on-screen frame presentation are not observed, and fps depends on window cell count.",
        excluded: "Confirmed on-screen frame rate",
        backends: &["fire-v2-overlapped"],
        variants: &[None],
        shell_required: false,
        workload: "urn:tbench:workload:fire:v1",
    },
];

fn find_profile(
    metric: Option<&str>,
    unit: Option<&str>,
    profile_id: Option<&str>,
) -> Option<&'static Profile> {
    let (m, u, id) = (metric?, unit?, profile_id?);
    PROFILES
        .iter()
        .find(|p| p.metric == m && p.unit == u && p.profile_id == id)
}

/// The profile a result row belongs to: same metric and unit, a registered
/// backend, and a variant the profile defines.
pub(crate) fn profile_for_row(
    metric: &str,
    unit: &str,
    variant: Option<&str>,
    backend: &str,
) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| {
        p.metric == metric
            && p.unit == unit
            && p.backends.contains(&backend)
            && if p.profile_id == "org.terminalbench.fire.fps.geometry" {
                variant.is_some_and(is_fire_geometry_variant)
            } else {
                p.variants.contains(&variant)
            }
    })
}

fn is_fire_geometry_variant(value: &str) -> bool {
    let Some((width, rows)) = value.split_once('x') else {
        return false;
    };
    if width.is_empty()
        || rows.is_empty()
        || !width.bytes().all(|b| b.is_ascii_digit())
        || !rows.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    matches!(
        (width.parse::<usize>(), rows.parse::<usize>()),
        (Ok(1..=1000), Ok(1..=500))
    )
}

fn all_profile_ids() -> Vec<&'static str> {
    let mut ids: BTreeSet<&'static str> = BTreeSet::new();
    for p in PROFILES {
        ids.insert(p.profile_id);
    }
    ids.into_iter().collect()
}

pub(crate) fn canonical_proxy_note(metric: &str, profile_id: Option<&str>) -> Option<&'static str> {
    match (metric, profile_id) {
        ("render.throughput", _) => Some("Proxy only: measures producer-side PTY write completion; it does not observe terminal parsing, final-frame presentation, frame time, FPS, pixels presented, or complete rendering."),
        ("scroll.proxy", _) => Some("Proxy only: measures producer-side PTY write completion for the scroll workload; it does not observe scroll, redraw, display events, frame time, FPS, pixels presented, or complete rendering."),
        ("input.latency", _) => Some("Proxy only: measures a synthetic PTY echo round trip; it does not observe physical keypress, GUI input latency, screen response, keypress-to-display, or keypress-to-photon latency."),
        ("fire.fps", Some("org.terminalbench.fire.fps.geometry")) => Some("Proxy only: measures producer-completed frames for a truecolor repaint stream at the cell geometry recorded by variant; throughput includes overlapped frame composition and PTY writing and does not observe terminal parsing, compositing, or confirmed on-screen frame presentation, so this fps is not a measured render frame rate."),
        ("fire.fps", _) => Some("Proxy only: measures producer-side PTY write completion for a live window-sized truecolor repaint stream; it does not observe terminal parsing, compositing, or confirmed on-screen frame presentation, so this fps is a producer-side drain-rate proxy, not a measured render frame rate, and it scales inversely with window cell count so values are only comparable at matching window geometry."),
        _ => None,
    }
}

const TOP_KEYS: &[&str] = &[
    "$schema",
    "wire_version",
    "producer",
    "protocol",
    "run_context",
    "meta",
    "results",
    "extensions",
];
const TOP_REQUIRED: &[&str] = &[
    "$schema",
    "wire_version",
    "producer",
    "protocol",
    "run_context",
    "meta",
    "results",
];
const ENV_KEYS: &[&str] = &[
    "os",
    "architecture",
    "terminal",
    "shell",
    "machine_model",
    "power_source",
    "low_power_mode",
    "display_refresh_hz",
    "synchronized_output_supported",
];
const META_KEYS: &[&str] = &[
    "timestamp",
    "os",
    "runner",
    "terminal",
    "shell",
    "synchronized_output_supported",
    "frame_skip_risk_note",
    "power_source",
    "low_power_mode",
    "machine_model",
    "cpu",
    "gpu",
    "display_refresh_hz",
    "meta_notes",
];
const META_REQUIRED: &[&str] = &[
    "timestamp",
    "os",
    "runner",
    "terminal",
    "shell",
    "synchronized_output_supported",
];
const RESULT_KEYS: &[&str] = &[
    "metric",
    "shell",
    "unit",
    "median",
    "p95",
    "stddev",
    "runs",
    "outliers_removed",
    "backend",
    "variant",
    "notes",
    "samples",
    "measurement",
];
const RESULT_REQUIRED: &[&str] = &[
    "metric",
    "shell",
    "unit",
    "median",
    "p95",
    "stddev",
    "runs",
    "outliers_removed",
    "backend",
    "measurement",
];
const MEASUREMENT_KEYS: &[&str] = &[
    "profile_id",
    "profile_version",
    "classification",
    "method_id",
    "method_version",
    "scope",
    "observed_boundary",
    "unit",
    "direction",
    "limitations",
    "excluded_claims",
    "workload",
    "dimensions",
];

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

struct Diag {
    rule_id: String,
    pointer: String,
    expected: Value,
    actual: Value,
    remediation: String,
}

struct Diagnostics {
    items: Vec<Diag>,
    unsupported_wire: bool,
    unsupported_profile: bool,
}

fn bit_length_i128(v: u128) -> u32 {
    // v > 0 guaranteed by caller
    128 - v.leading_zeros()
}

fn bit_length_decimal(digits: &str) -> u32 {
    // Repeatedly halve the decimal magnitude, counting shifts to zero.
    let mut d: Vec<u8> = digits.bytes().map(|b| b - b'0').collect();
    let mut bits = 0u32;
    while !(d.len() == 1 && d[0] == 0) {
        let mut carry = 0u8;
        let mut out = Vec::with_capacity(d.len());
        for &dig in &d {
            let cur = carry * 10 + dig;
            out.push(cur / 2);
            carry = cur % 2;
        }
        // strip leading zeros
        let mut start = 0;
        while start + 1 < out.len() && out[start] == 0 {
            start += 1;
        }
        d = out[start..].to_vec();
        bits += 1;
    }
    bits
}

fn transform_actual(actual: Value) -> Value {
    match actual {
        Value::Object(_) => Value::Str("<dict>".into()),
        Value::Array(_) => Value::Str("<list>".into()),
        Value::Int(i) if i.unsigned_abs() > 1_000_000_000_000_000_000u128 => Value::Str(format!(
            "<integer bit_length={}>",
            bit_length_i128(i.unsigned_abs())
        )),
        Value::BigInt { digits, .. } => Value::Str(format!(
            "<integer bit_length={}>",
            bit_length_decimal(&digits)
        )),
        Value::Str(s) if s.chars().count() > 80 => {
            let head: String = s.chars().take(77).collect();
            Value::Str(format!("{}...", head))
        }
        other => other,
    }
}

impl Diagnostics {
    fn new() -> Self {
        Diagnostics {
            items: Vec::new(),
            unsupported_wire: false,
            unsupported_profile: false,
        }
    }

    fn add(
        &mut self,
        rule_id: &str,
        pointer: String,
        expected: Value,
        actual: Value,
        remediation: &str,
    ) {
        if self.items.len() >= 100 {
            return;
        }
        self.items.push(Diag {
            rule_id: rule_id.to_string(),
            pointer,
            expected,
            actual: transform_actual(actual),
            remediation: remediation.to_string(),
        });
    }
}

fn emit_diagnostics(d: &Diagnostics) {
    for item in &d.items {
        let obj = Value::Object(vec![
            ("rule_id".into(), Value::Str(item.rule_id.clone())),
            ("pointer".into(), Value::Str(item.pointer.clone())),
            ("expected".into(), item.expected.clone()),
            ("actual".into(), item.actual.clone()),
            ("remediation".into(), Value::Str(item.remediation.clone())),
        ]);
        eprintln!("{}", dumps(&obj, false));
    }
}

// ---------------------------------------------------------------------------
// Structural helpers
// ---------------------------------------------------------------------------

fn vs(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn str_list(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|s| Value::Str((*s).into())).collect())
}

fn check_object(
    value: &Value,
    pointer: &str,
    allowed: &[&str],
    required: &[&str],
    d: &mut Diagnostics,
) -> bool {
    if !value.is_object() {
        d.add(
            "TBV-STRUCT-TYPE",
            pointer.to_string(),
            vs("object"),
            vs(value.type_name()),
            "Use a JSON object at this location.",
        );
        return false;
    }
    let keys: BTreeSet<&str> = value.keys().into_iter().collect();
    let missing: BTreeSet<&str> = required
        .iter()
        .copied()
        .filter(|k| !keys.contains(k))
        .collect();
    for key in &missing {
        d.add(
            "TBV-STRUCT-REQUIRED",
            format!("{}/{}", pointer, pointer_token(key)),
            vs("present"),
            vs("missing"),
            &format!("Add the required {} member.", key),
        );
    }
    let unknown: BTreeSet<&str> = keys
        .iter()
        .copied()
        .filter(|k| !allowed.contains(k))
        .collect();
    for key in &unknown {
        d.add(
            "TBV-STRUCT-UNKNOWN",
            format!("{}/{}", pointer, pointer_token(key)),
            vs("declared core member"),
            vs("unknown member"),
            "Remove the member or place vendor data in the top-level reverse-DNS extensions object.",
        );
    }
    true
}

fn is_trimmed(value: &Value) -> bool {
    match value {
        Value::Str(s) => !s.is_empty() && s == s.trim() && !s.contains('\n') && !s.contains('\r'),
        _ => false,
    }
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_rfc3339(value: &Value) -> bool {
    let s = match value {
        Value::Str(s) => s.as_bytes(),
        _ => return false,
    };
    let n = s.len();
    let mut i = 0usize;
    let read = |s: &[u8], i: &mut usize, c: usize| -> Option<u32> {
        if *i + c > s.len() {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..c {
            let d = s[*i];
            if !d.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (d - b'0') as u32;
            *i += 1;
        }
        Some(v)
    };
    let lit = |s: &[u8], i: &mut usize, c: u8| -> bool {
        if *i < s.len() && s[*i] == c {
            *i += 1;
            true
        } else {
            false
        }
    };
    let year = match read(s, &mut i, 4) {
        Some(v) => v,
        None => return false,
    };
    if !lit(s, &mut i, b'-') {
        return false;
    }
    let month = match read(s, &mut i, 2) {
        Some(v) => v,
        None => return false,
    };
    if !lit(s, &mut i, b'-') {
        return false;
    }
    let day = match read(s, &mut i, 2) {
        Some(v) => v,
        None => return false,
    };
    if !lit(s, &mut i, b'T') {
        return false;
    }
    let hour = match read(s, &mut i, 2) {
        Some(v) => v,
        None => return false,
    };
    if !lit(s, &mut i, b':') {
        return false;
    }
    let minute = match read(s, &mut i, 2) {
        Some(v) => v,
        None => return false,
    };
    if !lit(s, &mut i, b':') {
        return false;
    }
    let second = match read(s, &mut i, 2) {
        Some(v) => v,
        None => return false,
    };
    if i < n && s[i] == b'.' {
        i += 1;
        let start = i;
        while i < n && s[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if i < n && s[i] == b'Z' {
        i += 1;
    } else if i < n && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
        let oh = match read(s, &mut i, 2) {
            Some(v) => v,
            None => return false,
        };
        if !lit(s, &mut i, b':') {
            return false;
        }
        let om = match read(s, &mut i, 2) {
            Some(v) => v,
            None => return false,
        };
        if oh > 23 || om > 59 {
            return false;
        }
    } else {
        return false;
    }
    if i != n {
        return false;
    }
    if year < 1 || !(1..=12).contains(&month) {
        return false;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    if day < 1 || day > days_in_month(year, month) {
        return false;
    }
    true
}

fn os_id_ok(s: &str) -> bool {
    matches!(
        s,
        "darwin" | "linux" | "freebsd" | "openbsd" | "netbsd" | "windows"
    ) || other_token(s)
}

fn arch_id_ok(s: &str) -> bool {
    matches!(s, "aarch64" | "x86_64") || other_token(s)
}

fn other_token(s: &str) -> bool {
    let rest = match s.strip_prefix("other:") {
        Some(r) => r,
        None => return false,
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
}

fn extension_name_ok(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    for label in labels {
        let bytes = label.as_bytes();
        if bytes.is_empty() {
            return false;
        }
        let good = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        if !good(bytes[0]) || !good(bytes[bytes.len() - 1]) {
            return false;
        }
        if !bytes.iter().all(|&b| good(b) || b == b'-') {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Fact / measurement validation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum FactKind {
    Str,
    Bool,
    Float,
}

impl FactKind {
    fn name(self) -> &'static str {
        match self {
            FactKind::Str => "str",
            FactKind::Bool => "bool",
            FactKind::Float => "float",
        }
    }
}

fn validate_fact(value: &Value, pointer: &str, kind: FactKind, d: &mut Diagnostics) {
    if !value.is_object() {
        d.add(
            "TBV-FACT-TYPE",
            pointer.to_string(),
            vs("known/unknown fact object"),
            vs(value.type_name()),
            "Encode the fact with an explicit status.",
        );
        return;
    }
    match value.get("status").and_then(Value::as_str) {
        Some("known") => {
            check_object(
                value,
                pointer,
                &["status", "value"],
                &["status", "value"],
                d,
            );
            let actual = value.getn("value");
            let valid = match kind {
                FactKind::Str => matches!(&actual, Value::Str(s) if !s.is_empty()),
                FactKind::Bool => matches!(actual, Value::Bool(_)),
                FactKind::Float => {
                    is_number(&actual) && numeric_f64(&actual).is_some_and(|x| x > 0.0)
                }
            };
            if !valid {
                d.add(
                    "TBV-FACT-VALUE",
                    format!("{}/value", pointer),
                    vs(kind.name()),
                    vs(actual.type_name()),
                    "Provide a non-empty value of the required type or mark the fact unknown.",
                );
            }
        }
        Some("unknown") => {
            check_object(
                value,
                pointer,
                &["status", "reason"],
                &["status", "reason"],
                d,
            );
            let reason = value.getn("reason");
            if !matches!(&reason, Value::Str(s) if !s.is_empty()) {
                d.add(
                    "TBV-FACT-REASON",
                    format!("{}/reason", pointer),
                    vs("non-empty string"),
                    reason,
                    "Explain why the fact is unavailable without inventing a value.",
                );
            }
        }
        _ => {
            d.add(
                "TBV-FACT-STATUS",
                format!("{}/status", pointer),
                vs("known or unknown"),
                value.getn("status"),
                "Set an explicit fact status.",
            );
        }
    }
}

fn validate_measurement(result: &Value, index: usize, d: &mut Diagnostics) {
    let base = format!("/results/{}", index);
    let measurement = result.getn("measurement");
    if !check_object(
        &measurement,
        &format!("{}/measurement", base),
        MEASUREMENT_KEYS,
        MEASUREMENT_KEYS,
        d,
    ) {
        return;
    }
    let metric = result.get("metric").and_then(Value::as_str);
    let unit = result.get("unit").and_then(Value::as_str);
    let profile_id = measurement.getn("profile_id");
    let p = find_profile(metric, unit, profile_id.as_str());
    let known_ids = all_profile_ids();
    let id_known = profile_id.as_str().is_some_and(|s| known_ids.contains(&s));
    if p.is_none() || !id_known {
        d.unsupported_profile = true;
        d.add(
            "TBV-PROFILE-UNSUPPORTED",
            format!("{}/measurement/profile_id", base),
            vs("supported v1 profile"),
            profile_id,
            "Use a published Reporting Specification v1 metric/profile pair.",
        );
        return;
    }
    let p = p.unwrap();
    if measurement.getn("profile_version").as_str() != Some("1.0.0") {
        d.unsupported_profile = true;
        d.add(
            "TBV-PROFILE-VERSION",
            format!("{}/measurement/profile_version", base),
            vs("1.0.0"),
            measurement.getn("profile_version"),
            "Use a supported immutable profile version.",
        );
    }
    for field in [
        "profile_id",
        "classification",
        "method_id",
        "method_version",
        "scope",
        "observed_boundary",
        "unit",
        "direction",
        "limitations",
        "excluded_claims",
    ] {
        let expected = p.field(field);
        if !json_eq(&measurement.getn(field), &expected) {
            let rule = if matches!(
                field,
                "classification"
                    | "scope"
                    | "observed_boundary"
                    | "limitations"
                    | "excluded_claims"
            ) {
                "TBV-CLAIM-BOUNDARY"
            } else {
                "TBV-PROFILE-MISMATCH"
            };
            d.add(
                rule,
                format!("{}/measurement/{}", base, field),
                expected,
                measurement.getn(field),
                "Copy the immutable profile field exactly; choose a different profile for different semantics.",
            );
        }
    }
    let backend = result.getn("backend");
    let variant = result.getn("variant");
    let shell = result.getn("shell");
    if !backend.as_str().is_some_and(|b| p.backends.contains(&b)) {
        d.add(
            "TBV-PROFILE-BACKEND",
            format!("{}/backend", base),
            str_list(p.backends),
            backend.clone(),
            "Use a backend registered for this measurement profile.",
        );
    }
    let variant_ok = if p.profile_id == "org.terminalbench.fire.fps.geometry" {
        variant.as_str().is_some_and(is_fire_geometry_variant)
    } else {
        p.variants.iter().any(|v| match v {
            None => matches!(variant, Value::Null),
            Some(s) => variant.as_str() == Some(*s),
        })
    };
    if !variant_ok {
        let allowed: Vec<Value> = if p.profile_id == "org.terminalbench.fire.fps.geometry" {
            vec![vs("<width>x<rows>, width 1..1000 and rows 1..500")]
        } else {
            p.variants
                .iter()
                .map(|v| match v {
                    None => Value::Null,
                    Some(s) => Value::Str((*s).into()),
                })
                .collect()
        };
        d.add(
            "TBV-PROFILE-VARIANT",
            format!("{}/variant", base),
            Value::Array(allowed),
            variant.clone(),
            "Use a profile-defined workload variant.",
        );
    }
    if p.shell_required && !matches!(&shell, Value::Str(s) if !s.is_empty()) {
        d.add(
            "TBV-PROFILE-SHELL",
            format!("{}/shell", base),
            vs("non-empty shell identity"),
            shell.clone(),
            "Record the measured shell.",
        );
    }
    if !p.shell_required && !matches!(shell, Value::Null) {
        d.add(
            "TBV-PROFILE-SHELL",
            format!("{}/shell", base),
            Value::Null,
            shell.clone(),
            "Use null for a shell-independent profile.",
        );
    }
    let dims = measurement.getn("dimensions");
    if check_object(
        &dims,
        &format!("{}/measurement/dimensions", base),
        &["shell", "backend", "variant"],
        &["shell", "backend", "variant"],
        d,
    ) {
        for (field, actual) in [
            ("shell", &shell),
            ("backend", &backend),
            ("variant", &variant),
        ] {
            if !json_eq(&dims.getn(field), actual) {
                d.add(
                    "TBV-DIMENSION-MISMATCH",
                    format!("{}/measurement/dimensions/{}", base, field),
                    actual.clone(),
                    dims.getn(field),
                    "Keep structured dimensions identical to the legacy identity fields.",
                );
            }
        }
    }
    let workload = measurement.getn("workload");
    if !workload.is_object() {
        d.add(
            "TBV-WORKLOAD-TYPE",
            format!("{}/measurement/workload", base),
            vs("known/unknown workload object"),
            vs(workload.type_name()),
            "Record workload identity or an explicit unknown reason.",
        );
    } else if workload.getn("status").as_str() == Some("known") {
        check_object(
            &workload,
            &format!("{}/measurement/workload", base),
            &["status", "id", "version"],
            &["status", "id", "version"],
            d,
        );
        let variant_str = py_str(&variant);
        let expected_id = p.workload.replace("{variant}", &variant_str);
        if workload.getn("id").as_str() != Some(expected_id.as_str())
            || workload.getn("version").as_str() != Some("1.0.0")
        {
            d.add(
                "TBV-WORKLOAD-MISMATCH",
                format!("{}/measurement/workload", base),
                Value::Object(vec![
                    ("id".into(), Value::Str(expected_id)),
                    ("version".into(), vs("1.0.0")),
                ]),
                vs("known workload differs"),
                "Use the immutable workload identity for this profile and variant.",
            );
        }
    } else if workload.getn("status").as_str() == Some("unknown") {
        check_object(
            &workload,
            &format!("{}/measurement/workload", base),
            &["status", "reason"],
            &["status", "reason"],
            d,
        );
        let reason = workload.getn("reason");
        if !matches!(&reason, Value::Str(s) if !s.is_empty()) {
            d.add(
                "TBV-WORKLOAD-REASON",
                format!("{}/measurement/workload/reason", base),
                vs("non-empty string"),
                reason,
                "Explain why workload identity is unavailable.",
            );
        }
    } else {
        d.add(
            "TBV-WORKLOAD-STATUS",
            format!("{}/measurement/workload/status", base),
            vs("known or unknown"),
            workload.getn("status"),
            "Set an explicit workload status.",
        );
    }
    let notes = result.getn("notes");
    let profile_id = result
        .getn("measurement")
        .getn("profile_id")
        .as_str()
        .map(str::to_string);
    if let Some(canonical) = metric.and_then(|m| canonical_proxy_note(m, profile_id.as_deref())) {
        if !matches!(notes, Value::Null) && !json_eq(&notes, &vs(canonical)) {
            d.add(
                "TBV-CLAIM-NOTE",
                format!("{}/notes", base),
                vs(canonical),
                vs("non-canonical proxy note"),
                "Omit notes or copy the profile-specific canonical safe note exactly.",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Report validation
// ---------------------------------------------------------------------------

fn validate_report(report: &Value) -> (i32, Diagnostics) {
    let mut d = Diagnostics::new();
    if !check_object(report, "", TOP_KEYS, TOP_REQUIRED, &mut d) {
        return (EXIT_CONFORMANCE, d);
    }
    if report.getn("$schema").as_str() != Some(SCHEMA_ID) {
        d.add(
            "TBV-WIRE-SCHEMA",
            "/$schema".into(),
            vs(SCHEMA_ID),
            report.getn("$schema"),
            "Use the canonical v1 schema URN.",
        );
    }
    let wire = report.getn("wire_version");
    if !matches!(wire, Value::Null) && wire.as_str() != Some("1.0.0") {
        d.unsupported_wire = true;
        d.add(
            "TBV-WIRE-MAJOR",
            "/wire_version".into(),
            vs("exact supported wire version 1.0.0"),
            wire,
            "Use wire 1.0.0 or a validator that explicitly supports the report version.",
        );
    }
    let producer = report.getn("producer");
    if check_object(
        &producer,
        "/producer",
        &["name", "version"],
        &["name", "version"],
        &mut d,
    ) {
        if !matches!(producer.getn("name"), Value::Str(ref s) if !s.is_empty()) {
            d.add(
                "TBV-PRODUCER-NAME",
                "/producer/name".into(),
                vs("non-empty string"),
                producer.getn("name"),
                "Name the report producer.",
            );
        }
        validate_fact(
            &producer.getn("version"),
            "/producer/version",
            FactKind::Str,
            &mut d,
        );
    }
    let protocol = report.getn("protocol");
    if check_object(
        &protocol,
        "/protocol",
        &["id", "version"],
        &["id", "version"],
        &mut d,
    ) && (protocol.getn("id").as_str() != Some(PROTOCOL_ID)
        || protocol.getn("version").as_str() != Some("1.0.0"))
    {
        d.add(
            "TBV-PROTOCOL",
            "/protocol".into(),
            Value::Object(vec![
                ("id".into(), vs(PROTOCOL_ID)),
                ("version".into(), vs("1.0.0")),
            ]),
            vs("mismatch"),
            "Use the v1 reporting protocol identity.",
        );
    }
    let run_context = report.getn("run_context");
    if check_object(
        &run_context,
        "/run_context",
        &["environment"],
        &["environment"],
        &mut d,
    ) {
        let env = run_context.getn("environment");
        if check_object(&env, "/run_context/environment", ENV_KEYS, ENV_KEYS, &mut d) {
            let mut sorted_env: Vec<&str> = ENV_KEYS.to_vec();
            sorted_env.sort_unstable();
            for key in sorted_env {
                let kind = if key == "low_power_mode" {
                    FactKind::Bool
                } else if key == "display_refresh_hz" {
                    FactKind::Float
                } else {
                    FactKind::Str
                };
                validate_fact(
                    &env.getn(key),
                    &format!("/run_context/environment/{}", key),
                    kind,
                    &mut d,
                );
            }
            let sync = env.getn("synchronized_output_supported");
            if sync.is_object() && sync.getn("status").as_str() == Some("known") {
                let val = sync.getn("value");
                let ok = val.as_str().is_some_and(|v| SYNC_STATUSES.contains(&v));
                if !ok {
                    d.add(
                        "TBV-ENV-SYNC",
                        "/run_context/environment/synchronized_output_supported/value".into(),
                        str_list(&["no_reply", "supported", "unsupported"]),
                        sync.getn("value"),
                        "Use the defined synchronized-output probe status vocabulary or mark it unknown.",
                    );
                }
            }
            for (key, is_os, rule, expected) in [
                ("os", true, "TBV-CONTROL-OS", "canonical OS ID"),
                (
                    "architecture",
                    false,
                    "TBV-CONTROL-ARCH",
                    "canonical architecture ID",
                ),
            ] {
                let fact = env.getn(key);
                if fact.is_object() && fact.getn("status").as_str() == Some("known") {
                    let value = fact.getn("value");
                    let sval = match &value {
                        Value::Null => String::new(),
                        other => py_str(other),
                    };
                    let matches = if is_os {
                        os_id_ok(&sval)
                    } else {
                        arch_id_ok(&sval)
                    };
                    if !matches {
                        d.add(
                            rule,
                            format!("/run_context/environment/{}/value", key),
                            vs(expected),
                            fact.getn("value"),
                            "Use the registered value or a lowercase other:<token> identifier.",
                        );
                    }
                }
            }
            let power = env.getn("power_source");
            if power.is_object() && power.getn("status").as_str() == Some("known") {
                let val = power.getn("value");
                let ok = val.as_str().is_some_and(|v| POWER_SOURCES.contains(&v));
                if !ok {
                    d.add(
                        "TBV-CONTROL-POWER",
                        "/run_context/environment/power_source/value".into(),
                        str_list(&["ac", "battery", "ups"]),
                        power.getn("value"),
                        "Use ac, battery, or ups; otherwise mark the control unknown.",
                    );
                }
            }
            let machine = env.getn("machine_model");
            if machine.is_object()
                && machine.getn("status").as_str() == Some("known")
                && !is_trimmed(&machine.getn("value"))
            {
                d.add(
                    "TBV-CONTROL-MACHINE",
                    "/run_context/environment/machine_model/value".into(),
                    vs("trimmed case-sensitive opaque ID"),
                    machine.getn("value"),
                    "Trim the platform or override machine identifier without changing case.",
                );
            }
        }
    }
    let meta = report.getn("meta");
    if check_object(&meta, "/meta", META_KEYS, META_REQUIRED, &mut d) {
        for field in ["timestamp", "os", "runner", "terminal", "shell"] {
            if !matches!(meta.getn(field), Value::Str(ref s) if !s.is_empty()) {
                d.add(
                    "TBV-META-TYPE",
                    format!("/meta/{}", field),
                    vs("non-empty string"),
                    meta.getn(field),
                    "Record the required legacy metadata value.",
                );
            }
        }
        if !is_rfc3339(&meta.getn("timestamp")) {
            d.add(
                "TBV-META-TIMESTAMP",
                "/meta/timestamp".into(),
                vs("RFC 3339 date-time with timezone"),
                meta.getn("timestamp"),
                "Record a real calendar date-time such as 2026-07-16T00:00:00Z.",
            );
        }
        let sync = meta.getn("synchronized_output_supported");
        if !sync.as_str().is_some_and(|v| SYNC_STATUSES.contains(&v)) {
            d.add(
                "TBV-META-SYNC",
                "/meta/synchronized_output_supported".into(),
                str_list(&["supported", "unsupported", "no_reply"]),
                meta.getn("synchronized_output_supported"),
                "Use the defined probe status vocabulary.",
            );
        }
        for field in [
            "frame_skip_risk_note",
            "power_source",
            "machine_model",
            "cpu",
            "gpu",
            "meta_notes",
        ] {
            if meta.get(field).is_some()
                && !matches!(meta.getn(field), Value::Str(ref s) if !s.is_empty())
            {
                d.add(
                    "TBV-META-TYPE",
                    format!("/meta/{}", field),
                    vs("non-empty string"),
                    meta.getn(field),
                    "Omit unavailable optional metadata or record a non-empty string.",
                );
            }
        }
        if meta.get("low_power_mode").is_some()
            && !matches!(meta.getn("low_power_mode"), Value::Bool(_))
        {
            d.add(
                "TBV-META-TYPE",
                "/meta/low_power_mode".into(),
                vs("boolean"),
                meta.getn("low_power_mode"),
                "Record a JSON boolean.",
            );
        }
        if meta.get("display_refresh_hz").is_some() {
            let v = meta.getn("display_refresh_hz");
            if !is_number(&v) || numeric_f64(&v).is_none_or(|x| x <= 0.0) {
                d.add(
                    "TBV-META-TYPE",
                    "/meta/display_refresh_hz".into(),
                    vs("positive finite number"),
                    meta.getn("display_refresh_hz"),
                    "Record a positive refresh rate or omit unavailable legacy metadata.",
                );
            }
        }
    }
    // context/meta parity
    if run_context.is_object() && run_context.getn("environment").is_object() && meta.is_object() {
        let env = run_context.getn("environment");
        let parity = [
            "os",
            "terminal",
            "shell",
            "machine_model",
            "power_source",
            "low_power_mode",
            "display_refresh_hz",
            "synchronized_output_supported",
        ];
        for name in parity {
            let fact = env.getn(name);
            if fact.is_object() && fact.getn("status").as_str() == Some("known") {
                let fact_value = fact.getn("value");
                let meta_value = meta.getn(name);
                let matches = if name == "display_refresh_hz"
                    && is_number(&fact_value)
                    && is_number(&meta_value)
                {
                    (numeric_f64(&fact_value).unwrap() - numeric_f64(&meta_value).unwrap()).abs()
                        <= REFRESH_TOLERANCE_HZ
                } else {
                    json_eq(&meta_value, &fact_value)
                };
                if !matches {
                    d.add(
                        "TBV-CONTEXT-PARITY",
                        format!("/run_context/environment/{}/value", name),
                        vs(&format!(
                            "equal to /meta/{} (within 0.1 Hz for display refresh)",
                            name
                        )),
                        fact_value,
                        "Keep the normative known fact aligned with its legacy metadata field.",
                    );
                }
            }
        }
    }
    // extensions
    match report.get("extensions") {
        None | Some(Value::Null) => {}
        Some(ext) => {
            if !ext.is_object() {
                d.add(
                    "TBV-EXTENSION-TYPE",
                    "/extensions".into(),
                    vs("object"),
                    vs(ext.type_name()),
                    "Use a reverse-DNS keyed object.",
                );
            } else {
                let mut names: Vec<&str> = ext.keys();
                names.sort_unstable();
                for name in names {
                    if !extension_name_ok(name) {
                        d.add(
                            "TBV-EXTENSION-NAMESPACE",
                            format!("/extensions/{}", pointer_token(name)),
                            vs("reverse-DNS name"),
                            vs(name),
                            "Use a reverse-DNS extension namespace such as com.example.tool.",
                        );
                    }
                }
            }
        }
    }
    // results
    let results = report.getn("results");
    let results_ok = matches!(&results, Value::Array(a) if !a.is_empty());
    if !results_ok {
        d.add(
            "TBV-RESULTS-NONEMPTY",
            "/results".into(),
            vs("non-empty array"),
            vs("empty or non-array"),
            "Include at least one measured result.",
        );
    } else {
        let items = results.as_array().unwrap();
        let mut identities: Vec<String> = Vec::new();
        for (index, result) in items.iter().enumerate() {
            let base = format!("/results/{}", index);
            if !check_object(result, &base, RESULT_KEYS, RESULT_REQUIRED, &mut d) {
                continue;
            }
            for field in ["median", "p95", "stddev"] {
                let value = result.getn(field);
                let over = numeric_f64(&value).is_some_and(|x| x.abs() > MAX_NUMBER);
                if over {
                    d.add(
                        "TBV-NUMBER-RANGE",
                        format!("{}/{}", base, field),
                        vs("absolute value <= 1e308"),
                        value,
                        "Record a bounded IEEE-754 interoperable number.",
                    );
                } else if !is_number(&value) || numeric_f64(&value).is_none_or(|x| x < 0.0) {
                    d.add(
                        "TBV-NUMBER-NONNEGATIVE",
                        format!("{}/{}", base, field),
                        vs("finite non-negative number <= 1e308"),
                        value,
                        "Record a finite bounded value greater than or equal to zero.",
                    );
                }
            }
            let median = result.getn("median");
            let p95 = result.getn("p95");
            if is_number(&median)
                && is_number(&p95)
                && numeric_f64(&p95).unwrap() < numeric_f64(&median).unwrap()
            {
                d.add(
                    "TBV-STAT-P95",
                    format!("{}/p95", base),
                    vs(">= median"),
                    p95,
                    "Recompute summary statistics from the retained trials.",
                );
            }
            let runs = result.getn("runs");
            let outliers = result.getn("outliers_removed");
            if !is_int(&runs) || numeric_f64(&runs).is_none_or(|x| x < 1.0) {
                d.add(
                    "TBV-RUNS",
                    format!("{}/runs", base),
                    vs("integer >= 1"),
                    runs.clone(),
                    "Record the number of retained trials.",
                );
            }
            let outliers_bad = !is_int(&outliers)
                || numeric_f64(&outliers).is_none_or(|x| x < 0.0)
                || (is_int(&runs)
                    && numeric_f64(&outliers).unwrap_or(0.0) > numeric_f64(&runs).unwrap_or(0.0));
            if outliers_bad {
                d.add(
                    "TBV-OUTLIERS",
                    format!("{}/outliers_removed", base),
                    vs("integer from 0 through runs"),
                    outliers.clone(),
                    "Record a plausible non-negative removed-trial count.",
                );
            }
            if let Some(samples) = result.get("samples") {
                if !matches!(samples, Value::Null) {
                    match samples.as_array() {
                        Some(arr) if !arr.is_empty() => {
                            if is_int(&runs)
                                && (arr.len() as f64) != numeric_f64(&runs).unwrap_or(-1.0)
                            {
                                d.add(
                                    "TBV-SAMPLE-COUNT",
                                    format!("{}/samples", base),
                                    vs(&format!("{} retained samples", py_str(&runs))),
                                    Value::Int(arr.len() as i128),
                                    "Keep samples and runs consistent.",
                                );
                            }
                            for (si, sample) in arr.iter().enumerate() {
                                let over =
                                    numeric_f64(sample).is_some_and(|x| x.abs() > MAX_NUMBER);
                                if over {
                                    d.add(
                                        "TBV-NUMBER-RANGE",
                                        format!("{}/samples/{}", base, si),
                                        vs("absolute value <= 1e308"),
                                        sample.clone(),
                                        "Record a bounded IEEE-754 interoperable number.",
                                    );
                                } else if !is_number(sample)
                                    || numeric_f64(sample).is_none_or(|x| x < 0.0)
                                {
                                    d.add(
                                        "TBV-SAMPLE-NUMBER",
                                        format!("{}/samples/{}", base, si),
                                        vs("finite non-negative number <= 1e308"),
                                        sample.clone(),
                                        "Remove or replace the invalid sample.",
                                    );
                                }
                            }
                        }
                        _ => {
                            d.add(
                                "TBV-SAMPLES",
                                format!("{}/samples", base),
                                vs("non-empty number array"),
                                vs("invalid"),
                                "Include retained non-negative samples or omit the member.",
                            );
                        }
                    }
                }
            }
            if !matches!(result.getn("backend"), Value::Str(ref s) if !s.is_empty()) {
                d.add(
                    "TBV-RESULT-TYPE",
                    format!("{}/backend", base),
                    vs("non-empty string"),
                    result.getn("backend"),
                    "Record the measurement backend.",
                );
            }
            if result.get("notes").is_some()
                && !matches!(result.getn("notes"), Value::Str(ref s) if !s.is_empty())
            {
                d.add(
                    "TBV-RESULT-TYPE",
                    format!("{}/notes", base),
                    vs("non-empty string"),
                    result.getn("notes"),
                    "Omit notes or provide a non-empty claim-safe description.",
                );
            }
            validate_measurement(result, index, &mut d);
            let measurement = result.getn("measurement");
            let profile_id = if measurement.is_object() {
                measurement.getn("profile_id")
            } else {
                Value::Null
            };
            let identity = composite_key(&[
                result.getn("metric"),
                result.getn("shell"),
                result.getn("unit"),
                result.getn("variant"),
                profile_id,
            ]);
            if identities.contains(&identity) {
                d.add(
                    "TBV-RESULT-DUPLICATE",
                    base.clone(),
                    vs("unique metric/shell/unit/variant/profile identity"),
                    vs("duplicate"),
                    "Merge or remove the duplicate result.",
                );
            }
            identities.push(identity);
        }
    }
    if d.unsupported_wire {
        return (EXIT_WIRE, d);
    }
    if d.unsupported_profile {
        return (EXIT_PROFILE, d);
    }
    if d.items.is_empty() {
        (EXIT_OK, d)
    } else {
        (EXIT_CONFORMANCE, d)
    }
}

// ---------------------------------------------------------------------------
// Pairwise comparison gate
// ---------------------------------------------------------------------------

fn hkey(v: &Value) -> String {
    match v {
        Value::Null => "N".into(),
        Value::Bool(b) => format!("B{}", b),
        Value::Int(i) => format!("I{}", i),
        Value::BigInt { neg, digits } => format!("I{}{}", if *neg { "-" } else { "" }, digits),
        Value::Float(f) => format!("F{}", py_float(*f)),
        Value::Str(s) => format!("S{}", s),
        Value::Array(a) => {
            let inner: Vec<String> = a.iter().map(hkey).collect();
            format!("A[{}]", inner.join(","))
        }
        Value::Object(o) => {
            let mut inner: Vec<String> = o
                .iter()
                .map(|(k, val)| format!("{}={}", k, hkey(val)))
                .collect();
            inner.sort();
            format!("O{{{}}}", inner.join(","))
        }
    }
}

fn composite_key(values: &[Value]) -> String {
    values.iter().map(hkey).collect::<Vec<_>>().join("\u{0}")
}

fn result_identity(item: &Value) -> Vec<Value> {
    vec![
        item.getn("metric"),
        item.getn("unit"),
        item.getn("shell"),
        item.getn("variant"),
    ]
}

struct Reason {
    rule_id: String,
    pointer: String,
    detail: String,
}

fn compare_reports(a: &Value, b: &Value) -> (String, Vec<Reason>) {
    let mut reasons: Vec<Reason> = Vec::new();
    let mut known_mismatch = false;
    let mut missing = false;

    let empty: Vec<Value> = Vec::new();
    let a_results = a
        .getn("results")
        .as_array()
        .cloned()
        .unwrap_or_else(|| empty.clone());
    let b_results = b
        .getn("results")
        .as_array()
        .cloned()
        .unwrap_or_else(|| empty.clone());

    // identity -> (items, result)
    let mut ar: Vec<(String, Vec<Value>, Value)> = Vec::new();
    for item in &a_results {
        let items = result_identity(item);
        ar.push((composite_key(&items), items, item.clone()));
    }
    let mut br: Vec<(String, Vec<Value>, Value)> = Vec::new();
    for item in &b_results {
        let items = result_identity(item);
        br.push((composite_key(&items), items, item.clone()));
    }

    let aset: BTreeSet<&String> = ar.iter().map(|(k, _, _)| k).collect();
    let bset: BTreeSet<&String> = br.iter().map(|(k, _, _)| k).collect();
    if aset != bset {
        known_mismatch = true;
        reasons.push(Reason {
            rule_id: "TBC-RESULT-SET".into(),
            pointer: "/results".into(),
            detail: "result identity sets differ".into(),
        });
    }

    // common identities, sorted by str(tuple)
    let mut common: Vec<&(String, Vec<Value>, Value)> =
        ar.iter().filter(|(k, _, _)| bset.contains(k)).collect();
    common.sort_by_key(|x| tuple_repr(&x.1));

    let mut required_env: BTreeSet<&str> = [
        "os",
        "architecture",
        "machine_model",
        "power_source",
        "low_power_mode",
    ]
    .into_iter()
    .collect();
    let metric_of = |items: &[Value]| items[0].as_str().map(|s| s.to_string());
    if common.iter().any(|(_, items, _)| {
        matches!(
            metric_of(items).as_deref(),
            Some("render.throughput" | "scroll.proxy" | "fire.fps")
        )
    }) {
        required_env.insert("display_refresh_hz");
        required_env.insert("synchronized_output_supported");
    }
    if common.iter().any(|(_, items, _)| {
        matches!(
            metric_of(items).as_deref(),
            Some("shell.startup" | "cmd.overhead")
        )
    }) {
        required_env.insert("shell");
    }

    let aenv = a.getn("run_context").getn("environment");
    let benv = b.getn("run_context").getn("environment");

    if aenv.getn("terminal").getn("status").as_str() != Some("known")
        || benv.getn("terminal").getn("status").as_str() != Some("known")
    {
        missing = true;
        reasons.push(Reason {
            rule_id: "TBC-SUBJECT-UNKNOWN".into(),
            pointer: "/run_context/environment/terminal".into(),
            detail: "terminal subject identity is unknown".into(),
        });
    }

    for field in &required_env {
        let af = aenv.getn(field);
        let bf = benv.getn(field);
        if af.getn("status").as_str() != Some("known")
            || bf.getn("status").as_str() != Some("known")
        {
            missing = true;
            reasons.push(Reason {
                rule_id: "TBC-ENV-UNKNOWN".into(),
                pointer: format!("/run_context/environment/{}", field),
                detail: "a required environment fact is unknown".into(),
            });
        } else if *field == "display_refresh_hz" {
            let av = numeric_f64(&af.getn("value")).unwrap_or(f64::NAN);
            let bv = numeric_f64(&bf.getn("value")).unwrap_or(f64::NAN);
            if (av - bv).abs() > REFRESH_TOLERANCE_HZ {
                known_mismatch = true;
                reasons.push(Reason {
                    rule_id: "TBC-ENV-MISMATCH".into(),
                    pointer: format!("/run_context/environment/{}", field),
                    detail: "known refresh rates differ by more than 0.1 Hz".into(),
                });
            }
        } else if !json_eq(&af.getn("value"), &bf.getn("value")) {
            known_mismatch = true;
            reasons.push(Reason {
                rule_id: "TBC-ENV-MISMATCH".into(),
                pointer: format!("/run_context/environment/{}", field),
                detail: "known environment values differ".into(),
            });
        }
    }

    // index br by key for lookup
    for (key, items, a_result) in &common {
        let b_result = &br.iter().find(|(k, _, _)| k == key).unwrap().2;
        let am = a_result.getn("measurement");
        let bm = b_result.getn("measurement");
        let joined: Vec<String> = items.iter().map(py_str).collect();
        let label = format!("/results/{}", pointer_token(&joined.join("|")));
        for field in [
            "profile_id",
            "profile_version",
            "method_id",
            "method_version",
            "unit",
            "dimensions",
        ] {
            if !json_eq(&am.getn(field), &bm.getn(field)) {
                known_mismatch = true;
                reasons.push(Reason {
                    rule_id: "TBC-MEASUREMENT-MISMATCH".into(),
                    pointer: format!("{}/measurement/{}", label, field),
                    detail: "known measurement contract values differ".into(),
                });
            }
        }
        let aw = am.getn("workload");
        let bw = bm.getn("workload");
        if aw.getn("status").as_str() != Some("known")
            || bw.getn("status").as_str() != Some("known")
        {
            missing = true;
            reasons.push(Reason {
                rule_id: "TBC-WORKLOAD-UNKNOWN".into(),
                pointer: format!("{}/measurement/workload", label),
                detail: "workload identity is unknown".into(),
            });
        } else if !json_eq(&aw.getn("id"), &bw.getn("id"))
            || !json_eq(&aw.getn("version"), &bw.getn("version"))
        {
            known_mismatch = true;
            reasons.push(Reason {
                rule_id: "TBC-WORKLOAD-MISMATCH".into(),
                pointer: format!("{}/measurement/workload", label),
                detail: "known workload identities differ".into(),
            });
        }
    }

    if known_mismatch {
        ("NOT_COMPARABLE".into(), reasons)
    } else if missing {
        ("UNDETERMINED".into(), reasons)
    } else {
        ("COMPARABLE".into(), reasons)
    }
}

fn tuple_repr(items: &[Value]) -> String {
    let parts: Vec<String> = items.iter().map(py_repr).collect();
    format!("({})", parts.join(", "))
}

// ---------------------------------------------------------------------------
// I/O + CLI
// ---------------------------------------------------------------------------

fn emit_io_json(exc: &str) {
    let obj = Value::Object(vec![
        ("rule_id".into(), vs("TBV-IO-JSON")),
        ("pointer".into(), vs("")),
        ("expected".into(), vs("readable strict JSON")),
        ("actual".into(), vs(exc)),
        (
            "remediation".into(),
            vs("Provide a readable UTF-8 JSON report without NaN or Infinity."),
        ),
    ]);
    eprintln!("{}", dumps(&obj, false));
}

fn emit_dup(key: String) {
    let shown = if key.chars().count() <= 80 {
        key
    } else {
        let head: String = key.chars().take(77).collect();
        format!("{}...", head)
    };
    let obj = Value::Object(vec![
        ("rule_id".into(), vs("TBV-IO-DUPLICATE")),
        ("pointer".into(), vs("")),
        ("expected".into(), vs("unique keys in every JSON object")),
        (
            "actual".into(),
            Value::Str(format!("duplicate key: {}", shown)),
        ),
        (
            "remediation".into(),
            vs("Remove the duplicate member; key order never resolves ambiguity."),
        ),
    ]);
    eprintln!("{}", dumps(&obj, false));
}

fn os_error_name(err: &std::io::Error) -> &'static str {
    match err.raw_os_error() {
        Some(2) => "FileNotFoundError",
        Some(13) => "PermissionError",
        Some(20) => "NotADirectoryError",
        Some(21) => "IsADirectoryError",
        _ => "OSError",
    }
}

fn load(path: &str) -> Result<Value, ()> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            emit_io_json(os_error_name(&e));
            return Err(());
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => {
            emit_io_json("UnicodeDecodeError");
            return Err(());
        }
    };
    match parse(text) {
        Ok(v) => Ok(v),
        Err(ParseErr::Duplicate(k)) => {
            emit_dup(k);
            Err(())
        }
        Err(ParseErr::NonFinite) => {
            emit_io_json("ValueError");
            Err(())
        }
        Err(ParseErr::Syntax) => {
            emit_io_json("JSONDecodeError");
            Err(())
        }
    }
}

fn usage() {
    println!(
        "Usage: tbench-validate validate REPORT\n       tbench-validate compare REPORT_A REPORT_B\n       tbench-validate profiles\n\nSafe comparison: validate both reports, run this pairwise compare gate, then use bin/tbench compare only after COMPARABLE.\nSpecification and copy-paste quickstart: docs/reporting-spec-v1.md\n\nExit codes: 0 success; 2 usage; 3 I/O/JSON; 4 conformance; 5 unsupported wire; 6 unsupported profile; 7 not comparable; 8 undetermined."
    );
}

fn print_profiles() {
    let ids = all_profile_ids();
    let mut out = String::from("[\n");
    for (idx, id) in ids.iter().enumerate() {
        if idx > 0 {
            out.push_str(",\n");
        }
        out.push_str("  ");
        let mut s = String::new();
        escape_str(id, &mut s);
        out.push_str(&s);
    }
    out.push_str("\n]");
    println!("{}", out);
}

fn code_to_exit(code: i32) -> ExitCode {
    ExitCode::from(code as u8)
}

pub fn cmd_validate(args: &[String], _root: &Path) -> ExitCode {
    let argv = args;
    if argv.is_empty() || matches!(argv[0].as_str(), "-h" | "--help" | "help") {
        usage();
        return code_to_exit(if argv.is_empty() { EXIT_USAGE } else { EXIT_OK });
    }
    if argv[0] == "profiles" && argv.len() == 1 {
        print_profiles();
        return code_to_exit(EXIT_OK);
    }
    if argv[0] == "validate" && argv.len() == 2 {
        let report = match load(&argv[1]) {
            Ok(r) => r,
            Err(()) => return code_to_exit(EXIT_IO),
        };
        let (code, d) = validate_report(&report);
        if code != 0 {
            emit_diagnostics(&d);
            return code_to_exit(code);
        }
        let n = report
            .getn("results")
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        let obj = Value::Object(vec![
            ("status".into(), vs("CONFORMANT")),
            ("schema".into(), vs(SCHEMA_ID)),
            ("results".into(), Value::Int(n as i128)),
        ]);
        println!("{}", dumps(&obj, true));
        return code_to_exit(EXIT_OK);
    }
    if argv[0] == "compare" && argv.len() == 3 {
        let mut loaded: Vec<Value> = Vec::new();
        for path in &argv[1..3] {
            let report = match load(path) {
                Ok(r) => r,
                Err(()) => return code_to_exit(EXIT_IO),
            };
            let (code, d) = validate_report(&report);
            if code != 0 {
                emit_diagnostics(&d);
                return code_to_exit(code);
            }
            loaded.push(report);
        }
        let (status, reasons) = compare_reports(&loaded[0], &loaded[1]);
        let reason_arr: Vec<Value> = reasons
            .iter()
            .map(|r| {
                Value::Object(vec![
                    ("rule_id".into(), Value::Str(r.rule_id.clone())),
                    ("pointer".into(), Value::Str(r.pointer.clone())),
                    ("detail".into(), Value::Str(r.detail.clone())),
                ])
            })
            .collect();
        let obj = Value::Object(vec![
            ("status".into(), Value::Str(status.clone())),
            ("reasons".into(), Value::Array(reason_arr)),
        ]);
        println!("{}", dumps(&obj, false));
        let code = match status.as_str() {
            "COMPARABLE" => EXIT_OK,
            "NOT_COMPARABLE" => EXIT_NOT_COMPARABLE,
            _ => EXIT_UNDETERMINED,
        };
        return code_to_exit(code);
    }
    usage();
    code_to_exit(EXIT_USAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fire_geometry_variant_is_strict_and_bounded() {
        for valid in ["1x1", "80x24", "1000x500"] {
            assert!(is_fire_geometry_variant(valid), "{valid}");
        }
        for invalid in [
            "", "80", "80X24", "0x24", "80x0", "1001x24", "80x501", "80x24x1", " 80x24",
        ] {
            assert!(!is_fire_geometry_variant(invalid), "{invalid}");
        }
    }

    #[test]
    fn rows_resolve_to_one_registered_profile() {
        let id = |m, u, v, b| profile_for_row(m, u, v, b).map(|p| p.profile_id);
        assert_eq!(
            id("resource.cpu", "pct", Some("idle"), "ps"),
            Some("org.terminalbench.resource.cpu")
        );
        assert_eq!(
            id("resource.cpu", "pct", Some("idle"), "procfs"),
            Some("org.terminalbench.resource.cpu.window")
        );
        assert_eq!(
            id("fire.fps", "fps", Some("80x24"), "fire-v2-overlapped"),
            Some("org.terminalbench.fire.fps.geometry")
        );
        assert_eq!(
            id("fire.fps", "fps", None, "fire-v2-overlapped"),
            Some("org.terminalbench.fire.fps")
        );
        assert_eq!(
            id("render.throughput", "MBps", Some("plain"), "rust-writer"),
            Some("org.terminalbench.render.throughput.bytes")
        );
        assert_eq!(id("render.throughput", "MBps", Some("plain"), "cat"), None);
        assert_eq!(
            id("render.throughput", "MBps", Some("bold"), "rust-writer"),
            None
        );
        assert_eq!(id("made.up", "ms", None, "rust-timer"), None);
    }

    /// The specification is the normative text and the schema is what
    /// third parties validate against. Both restate the registry, so both
    /// are checked against it here.
    #[test]
    fn specification_restates_every_profile_exactly() {
        let spec = include_str!("../docs/reporting-spec-v1.md");
        for p in PROFILES {
            for needle in [
                p.profile_id,
                p.method_id,
                p.scope,
                p.observed_boundary,
                p.limitation,
                p.excluded,
                p.workload,
            ] {
                assert!(
                    spec.contains(needle),
                    "spec lacks {needle:?} ({})",
                    p.profile_id
                );
            }
            let row = spec
                .lines()
                .find(|l| l.starts_with("| `") && l.contains(&format!("`{}` |", p.profile_id)))
                .unwrap_or_else(|| panic!("no registry row for {}", p.profile_id));
            for backend in p.backends {
                assert!(
                    row.contains(&format!("`{backend}`")),
                    "{}: backend {backend}",
                    p.profile_id
                );
            }
            assert!(
                row.starts_with(&format!("| `{}` / `{}` |", p.metric, p.unit)),
                "{row}"
            );
        }
        for metric in [
            "render.throughput",
            "scroll.proxy",
            "input.latency",
            "fire.fps",
        ] {
            for p in PROFILES.iter().filter(|p| p.metric == metric) {
                let note = canonical_proxy_note(metric, Some(p.profile_id)).unwrap();
                assert!(
                    spec.contains(note),
                    "spec lacks the note for {}",
                    p.profile_id
                );
            }
        }
    }

    #[test]
    fn schema_enumerates_the_registry_metrics_units_and_notes() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/tbench-report-v1.schema.json")).unwrap();
        let result = &schema["$defs"]["result"];
        let strings = |v: &serde_json::Value| -> BTreeSet<String> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect()
        };
        let metrics: BTreeSet<String> = PROFILES.iter().map(|p| p.metric.to_string()).collect();
        let units: BTreeSet<String> = PROFILES.iter().map(|p| p.unit.to_string()).collect();
        assert_eq!(strings(&result["properties"]["metric"]["enum"]), metrics);
        assert_eq!(strings(&result["properties"]["unit"]["enum"]), units);
        assert_eq!(
            strings(&schema["$defs"]["measurement"]["properties"]["unit"]["enum"]),
            units
        );

        for rule in result["allOf"].as_array().unwrap() {
            let metric = rule["if"]["properties"]["metric"]["const"]
                .as_str()
                .unwrap();
            let notes = &rule["then"]["properties"]["notes"];
            let allowed: BTreeSet<String> = match notes.get("const") {
                Some(one) => [one.as_str().unwrap().to_string()].into(),
                None => strings(&notes["enum"]),
            };
            let registered: BTreeSet<String> = PROFILES
                .iter()
                .filter(|p| p.metric == metric)
                .filter_map(|p| canonical_proxy_note(metric, Some(p.profile_id)))
                .map(str::to_string)
                .collect();
            assert_eq!(allowed, registered, "{metric}");
        }
    }

    #[test]
    fn pointer_escaping() {
        assert_eq!(pointer_token("a/b"), "a~1b");
        assert_eq!(pointer_token("a~b"), "a~0b");
        assert_eq!(pointer_token("~/"), "~0~1");
        assert_eq!(pointer_token("plain"), "plain");
    }

    #[test]
    fn duplicate_key_detection() {
        match parse("{\"a\":1,\"a\":2}") {
            Err(ParseErr::Duplicate(k)) => assert_eq!(k, "a"),
            _ => panic!("expected duplicate"),
        }
        // nested duplicate reported before outer completion
        match parse("{\"x\":{\"b\":1,\"b\":2}}") {
            Err(ParseErr::Duplicate(k)) => assert_eq!(k, "b"),
            _ => panic!("expected nested duplicate"),
        }
        assert!(parse("{\"a\":1,\"b\":2}").is_ok());
    }

    #[test]
    fn non_finite_rejected() {
        assert!(matches!(parse("{\"x\": NaN}"), Err(ParseErr::NonFinite)));
        assert!(matches!(
            parse("{\"x\": Infinity}"),
            Err(ParseErr::NonFinite)
        ));
        assert!(matches!(
            parse("{\"x\": -Infinity}"),
            Err(ParseErr::NonFinite)
        ));
    }

    #[test]
    fn python_float_formatting() {
        assert_eq!(py_float(1.5), "1.5");
        assert_eq!(py_float(100.0), "100.0");
        assert_eq!(py_float(0.1), "0.1");
        assert_eq!(py_float(0.0001), "0.0001");
        assert_eq!(py_float(0.00001), "1e-05");
        assert_eq!(py_float(1e16), "1e+16");
        assert_eq!(py_float(1e15), "1000000000000000.0");
        assert_eq!(py_float(60.0), "60.0");
        assert_eq!(py_float(-0.0), "-0.0");
    }

    #[test]
    fn conformance_rule_claim_boundary() {
        // proxy render profile with wrong classification triggers TBV-CLAIM-BOUNDARY
        let json = r#"{
          "$schema":"urn:tbench:schema:report:v1",
          "wire_version":"1.0.0",
          "producer":{"name":"x","version":{"status":"unknown","reason":"n/a"}},
          "protocol":{"id":"urn:tbench:protocol:reporting","version":"1.0.0"},
          "run_context":{"environment":{
             "os":{"status":"unknown","reason":"n/a"},
             "architecture":{"status":"unknown","reason":"n/a"},
             "terminal":{"status":"unknown","reason":"n/a"},
             "shell":{"status":"unknown","reason":"n/a"},
             "machine_model":{"status":"unknown","reason":"n/a"},
             "power_source":{"status":"unknown","reason":"n/a"},
             "low_power_mode":{"status":"unknown","reason":"n/a"},
             "display_refresh_hz":{"status":"unknown","reason":"n/a"},
             "synchronized_output_supported":{"status":"unknown","reason":"n/a"}
          }},
          "meta":{"timestamp":"2026-07-16T00:00:00Z","os":"o","runner":"r","terminal":"t","shell":"s","synchronized_output_supported":"no_reply"},
          "results":[{
             "metric":"render.throughput","shell":null,"unit":"MBps","median":1.0,"p95":1.0,"stddev":0.0,
             "runs":1,"outliers_removed":0,"backend":"zsh-timer","variant":"plain",
             "measurement":{
                "profile_id":"org.terminalbench.render.throughput.bytes","profile_version":"1.0.0",
                "classification":"direct-observation","method_id":"org.terminalbench.method.pty-write-wall-clock",
                "method_version":"1.0.0","scope":"pty-output-pipeline","observed_boundary":"fixture-write-start-to-producer-exit",
                "unit":"MBps","direction":"higher-is-better",
                "limitations":["Measures producer-side PTY write completion; terminal parsing, final-frame presentation, and frame skipping are not observed."],
                "excluded_claims":["On-screen rendering throughput"],
                "workload":{"status":"known","id":"urn:tbench:workload:render-plain:v1","version":"1.0.0"},
                "dimensions":{"shell":null,"backend":"zsh-timer","variant":"plain"}
             }
          }]
        }"#;
        let report = parse(json).expect("valid json");
        let (code, d) = validate_report(&report);
        assert_eq!(code, EXIT_CONFORMANCE);
        assert!(d.items.iter().any(|i| i.rule_id == "TBV-CLAIM-BOUNDARY"
            && i.pointer == "/results/0/measurement/classification"));
    }
}
