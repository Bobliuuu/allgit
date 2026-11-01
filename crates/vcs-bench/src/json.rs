//! Minimal JSON value + writer (no serde dependency in the benchmark crate).

use std::fmt::Write;

#[derive(Clone, Debug)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn s(v: impl Into<String>) -> J {
        J::Str(v.into())
    }
    pub fn n(v: impl Into<f64>) -> J {
        J::Num(v.into())
    }
    pub fn obj(items: Vec<(&str, J)>) -> J {
        J::Obj(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn write(&self, out: &mut String, indent: usize, pretty: bool) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => {
                if n.is_finite() {
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        let _ = write!(out, "{}", *n as i64);
                    } else {
                        let _ = write!(out, "{:.6}", n);
                    }
                } else {
                    out.push_str("null")
                }
            }
            J::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            J::Arr(a) => {
                if a.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                // arrays of scalars stay on one line
                let flat = a.iter().all(|x| !matches!(x, J::Arr(_) | J::Obj(_)));
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                        if flat || !pretty {
                            out.push(' ');
                        }
                    }
                    if pretty && !flat {
                        out.push('\n');
                        out.push_str(&"  ".repeat(indent + 1));
                    }
                    x.write(out, indent + 1, pretty);
                }
                if pretty && !flat {
                    out.push('\n');
                    out.push_str(&"  ".repeat(indent));
                }
                out.push(']');
            }
            J::Obj(o) => {
                if o.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if pretty {
                        out.push('\n');
                        out.push_str(&"  ".repeat(indent + 1));
                    }
                    J::Str(k.clone()).write(out, 0, false);
                    out.push_str(if pretty { ": " } else { ":" });
                    v.write(out, indent + 1, pretty);
                }
                if pretty {
                    out.push('\n');
                    out.push_str(&"  ".repeat(indent));
                }
                out.push('}');
            }
        }
    }
    pub fn pretty(&self) -> String {
        let mut s = String::new();
        self.write(&mut s, 0, true);
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_and_nesting() {
        let j = J::obj(vec![
            ("a\"b", J::s("x\ny")),
            ("n", J::Arr(vec![J::n(1.0), J::n(2.5)])),
            ("z", J::Null),
        ]);
        let s = j.pretty();
        assert!(s.contains("\"a\\\"b\": \"x\\ny\""));
        assert!(s.contains("[1, 2.500000]"));
    }
}
