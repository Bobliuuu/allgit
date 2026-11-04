//! Deterministic generator for the structural-vs-text merge corpus (no randomness source, no files).

/// Language flavour of the generated files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Rust,
    Python,
    TypeScript,
    Go,
}

impl Style {
    pub const ALL: [Style; 4] = [Style::Rust, Style::Python, Style::TypeScript, Style::Go];

    pub fn name(self) -> &'static str {
        match self {
            Style::Rust => "rust",
            Style::Python => "python",
            Style::TypeScript => "typescript",
            Style::Go => "go",
        }
    }

    fn comment(self) -> &'static str {
        if self == Style::Python {
            "#"
        } else {
            "//"
        }
    }

    /// Lines after the last body line (the closing brace); Python has none.
    fn close_lines(self) -> usize {
        usize::from(self != Style::Python)
    }
}

/// One three-way merge input.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub kind: &'static str,
    pub base: Vec<u8>,
    pub left: Vec<u8>,
    pub right: Vec<u8>,
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

/// Items of the generated file: imports, a type, free functions, then methods (inside an
/// `impl`/`class`, or as top-level functions in Go).
struct Model {
    fns: Vec<String>,
    methods: Vec<String>,
}

fn func(st: Style, name: &str, method: bool, rng: &mut Lcg) -> String {
    let n = 3 + rng.next(3);
    let mut vals = Vec::new();
    for _ in 0..n {
        vals.push(rng.next(100));
    }
    let ind = if method && st != Style::Go { "    " } else { "" };
    let mut s = String::new();
    match st {
        Style::Rust => {
            s += &format!("{ind}fn {name}(a: i32) -> i32 {{\n");
            for (i, x) in vals.iter().enumerate() {
                s += &format!("{ind}    let v{i} = a + {x};\n");
            }
            s += &format!("{ind}    v{}\n{ind}}}\n", n - 1);
        }
        Style::Python => {
            let sig = if method { format!("{name}(self, a)") } else { format!("{name}(a)") };
            s += &format!("{ind}def {sig}:\n");
            for (i, x) in vals.iter().enumerate() {
                s += &format!("{ind}    v{i} = a + {x}\n");
            }
            s += &format!("{ind}    return v{}\n", n - 1);
        }
        Style::TypeScript => {
            if method {
                s += &format!("{ind}  {name}(a: number): number {{\n");
            } else {
                s += &format!("export function {name}(a: number): number {{\n");
            }
            let pad = if method { "    " } else { "  " };
            for (i, x) in vals.iter().enumerate() {
                s += &format!("{pad}const v{i} = a + {x};\n");
            }
            s += &format!("{pad}return v{};\n{}}}\n", n - 1, if method { "  " } else { "" });
        }
        Style::Go => {
            if method {
                s += &format!("func (w *Widget) {name}(a int) int {{\n");
            } else {
                s += &format!("func {name}(a int) int {{\n");
            }
            for (i, x) in vals.iter().enumerate() {
                s += &format!("\tv{i} := a + {x}\n");
            }
            s += &format!("\treturn v{}\n}}\n", n - 1);
        }
    }
    s
}

fn model(st: Style, rng: &mut Lcg) -> Model {
    let nf = 5 + rng.next(4);
    let nm = 3 + rng.next(3);
    Model {
        fns: (0..nf).map(|i| func(st, &format!("f{i}"), false, rng)).collect(),
        methods: (0..nm).map(|i| func(st, &format!("m{i}"), true, rng)).collect(),
    }
}

fn render(st: Style, m: &Model) -> String {
    let mut parts: Vec<String> = match st {
        Style::Rust => vec!["use std::fmt;\n".into(), "pub struct Widget {\n    pub n: i32,\n}\n".into()],
        Style::Python => vec!["import os\n".into()],
        Style::TypeScript => vec![
            "import { fmt } from \"./fmt\";\n".into(),
            "export interface Widget {\n  n: number;\n}\n".into(),
        ],
        Style::Go => vec![
            "package main\n".into(),
            "import \"fmt\"\n".into(),
            "type Widget struct {\n\tN int\n}\n".into(),
        ],
    };
    parts.extend(m.fns.iter().cloned());
    match st {
        Style::Rust => parts.push(format!("impl Widget {{\n{}}}\n", m.methods.join("\n"))),
        Style::Python => parts.push(format!("class Widget:\n{}", m.methods.join("\n"))),
        Style::TypeScript => parts.push(format!("export class Widget {{\n{}}}\n", m.methods.join("\n"))),
        Style::Go => parts.extend(m.methods.iter().cloned()),
    }
    parts.join("\n")
}

/// Rewrite one body line of an item (line 1 is the first statement).
fn edit_line(st: Style, item: &str, line: usize, tag: &str) -> String {
    let mut ls: Vec<String> = item.lines().map(str::to_string).collect();
    let i = line.min(ls.len() - 1 - st.close_lines());
    ls[i] = format!("{} {} {tag}", ls[i], st.comment());
    ls.join("\n") + "\n"
}

fn edit_first(st: Style, item: &str, tag: &str) -> String {
    edit_line(st, item, 1, tag)
}

fn edit_last(st: Style, item: &str, tag: &str) -> String {
    let n = item.lines().count();
    edit_line(st, item, n - 1 - st.close_lines(), tag)
}

/// A syntax error injected in the first line of the body.
fn break_syntax(st: Style, item: &str) -> String {
    match st {
        Style::Python => item.replacen(":\n", ":\n    = ;\n", 1),
        Style::Rust => item.replacen("{\n", "{\n    let = ;\n", 1),
        _ => item.replacen("{\n", "{\n    = ;\n", 1),
    }
}

/// Rust scenarios (see [`generate_for`]).
pub fn generate(per_kind: usize) -> Vec<Scenario> {
    generate_for(Style::Rust, per_kind)
}

/// Generate `per_kind` scenarios of every kind (deterministic for a given `per_kind`).
pub fn generate_for(st: Style, per_kind: usize) -> Vec<Scenario> {
    let mut out = Vec::new();
    let mut rng = Lcg(0x5eed);
    let bytes = |m: &Model| render(st, m).into_bytes();
    for _ in 0..per_kind {
        let base = model(st, &mut rng);
        let nf = base.fns.len();
        let nm = base.methods.len();
        let a = rng.next(nf - 1);
        let b = a + 1 + rng.next(nf - a - 1); // b > a
        let mut push = |kind: &'static str, l: &Model, r: &Model| {
            out.push(Scenario {
                kind,
                base: bytes(&base),
                left: bytes(l),
                right: bytes(r),
            })
        };
        let dup = |m: &Model| Model {
            fns: m.fns.clone(),
            methods: m.methods.clone(),
        };

        // Edits to two different functions.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns[a] = edit_first(st, &l.fns[a], "left");
        r.fns[b] = edit_last(st, &r.fns[b], "right");
        push("edit_different_fns", &l, &r);

        // Left moves a function to the end of the free functions, right edits it.
        let (mut l, mut r) = (dup(&base), dup(&base));
        let moved = l.fns.remove(a);
        l.fns.push(moved);
        r.fns[a] = edit_first(st, &r.fns[a], "right");
        push("move_fn_vs_edit_it", &l, &r);

        // Left moves a function, right edits another one.
        let (mut l, mut r) = (dup(&base), dup(&base));
        let moved = l.fns.remove(a);
        l.fns.push(moved);
        r.fns[b] = edit_first(st, &r.fns[b], "right");
        push("move_fn_vs_edit_other", &l, &r);

        // Left appends a method to the impl, right edits the last method's last line.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.methods.push(func(st, "added", true, &mut rng));
        r.methods[nm - 1] = edit_last(st, &r.methods[nm - 1], "right");
        push("add_method_vs_edit_last", &l, &r);

        // Left edits method i, right inserts a new method right after it.
        let i = rng.next(nm);
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.methods[i] = edit_last(st, &l.methods[i], "left");
        r.methods.insert(i + 1, func(st, "inserted", true, &mut rng));
        push("edit_method_vs_insert_after", &l, &r);

        // Left deletes one function, right edits another.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns.remove(a);
        r.fns[b] = edit_first(st, &r.fns[b], "right");
        push("delete_fn_vs_edit_other", &l, &r);

        // Both edit methods of the same impl.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.methods[0] = edit_first(st, &l.methods[0], "left");
        r.methods[nm - 1] = edit_last(st, &r.methods[nm - 1], "right");
        push("edit_two_methods", &l, &r);

        // Genuine conflicts: same line of the same function.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns[a] = edit_first(st, &l.fns[a], "left");
        r.fns[a] = edit_first(st, &r.fns[a], "right");
        push("same_line_conflict", &l, &r);

        // Genuine conflicts: delete vs edit of one function.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns.remove(a);
        r.fns[a] = edit_first(st, &r.fns[a], "right");
        push("delete_vs_edit_same_fn", &l, &r);

        // Genuine conflicts: both add a different function after the last one.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns.push(func(st, "left_new", false, &mut rng));
        r.fns.push(func(st, "right_new", false, &mut rng));
        push("add_fn_both_at_end", &l, &r);

        // One side does not parse: structural must fall back.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns[a] = break_syntax(st, &l.fns[a]);
        r.fns[b] = edit_first(st, &r.fns[b], "right");
        push("left_syntax_error", &l, &r);
    }
    out
}
