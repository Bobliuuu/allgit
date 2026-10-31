//! Deterministic generator for the structural-vs-text merge corpus (no randomness source, no files).

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

/// Items of the generated file: `use`, a struct, free functions, then an `impl` with methods.
struct Model {
    fns: Vec<String>,
    methods: Vec<String>,
}

fn func(name: &str, indent: &str, rng: &mut Lcg) -> String {
    let n = 3 + rng.next(3);
    let mut s = format!("{indent}fn {name}(a: i32) -> i32 {{\n");
    for i in 0..n {
        s += &format!("{indent}    let v{i} = a + {};\n", rng.next(100));
    }
    s += &format!("{indent}    v{}\n{indent}}}\n", n - 1);
    s
}

fn model(rng: &mut Lcg) -> Model {
    let nf = 5 + rng.next(4);
    let nm = 3 + rng.next(3);
    Model {
        fns: (0..nf).map(|i| func(&format!("f{i}"), "", rng)).collect(),
        methods: (0..nm).map(|i| func(&format!("m{i}"), "    ", rng)).collect(),
    }
}

fn render(m: &Model) -> String {
    let mut parts = vec![
        "use std::fmt;\n".to_string(),
        "pub struct Widget {\n    pub n: i32,\n}\n".to_string(),
    ];
    parts.extend(m.fns.iter().cloned());
    parts.push(format!("impl Widget {{\n{}}}\n", m.methods.join("\n")));
    parts.join("\n")
}

/// Rewrite one body line of an item (line 1 is the first statement).
fn edit_line(item: &str, line: usize, tag: &str) -> String {
    let mut ls: Vec<String> = item.lines().map(str::to_string).collect();
    let i = line.min(ls.len() - 2);
    ls[i] = format!("{} // {tag}", ls[i]);
    ls.join("\n") + "\n"
}

fn edit_first(item: &str, tag: &str) -> String {
    edit_line(item, 1, tag)
}

fn edit_last(item: &str, tag: &str) -> String {
    let n = item.lines().count();
    edit_line(item, n - 2, tag)
}

/// Generate `per_kind` scenarios of every kind (deterministic for a given `per_kind`).
pub fn generate(per_kind: usize) -> Vec<Scenario> {
    let mut out = Vec::new();
    let mut rng = Lcg(0x5eed);
    let bytes = |m: &Model| render(m).into_bytes();
    for _ in 0..per_kind {
        let base = model(&mut rng);
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
        l.fns[a] = edit_first(&l.fns[a], "left");
        r.fns[b] = edit_last(&r.fns[b], "right");
        push("edit_different_fns", &l, &r);

        // Left moves a function to the end of the free functions, right edits it.
        let (mut l, mut r) = (dup(&base), dup(&base));
        let moved = l.fns.remove(a);
        l.fns.push(moved);
        r.fns[a] = edit_first(&r.fns[a], "right");
        push("move_fn_vs_edit_it", &l, &r);

        // Left moves a function, right edits another one.
        let (mut l, mut r) = (dup(&base), dup(&base));
        let moved = l.fns.remove(a);
        l.fns.push(moved);
        r.fns[b] = edit_first(&r.fns[b], "right");
        push("move_fn_vs_edit_other", &l, &r);

        // Left appends a method to the impl, right edits the last method's last line.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.methods.push(func("added", "    ", &mut rng));
        r.methods[nm - 1] = edit_last(&r.methods[nm - 1], "right");
        push("add_method_vs_edit_last", &l, &r);

        // Left edits method i, right inserts a new method right after it.
        let i = rng.next(nm);
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.methods[i] = edit_last(&l.methods[i], "left");
        r.methods.insert(i + 1, func("inserted", "    ", &mut rng));
        push("edit_method_vs_insert_after", &l, &r);

        // Left deletes one function, right edits another.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns.remove(a);
        r.fns[b] = edit_first(&r.fns[b], "right");
        push("delete_fn_vs_edit_other", &l, &r);

        // Both edit methods of the same impl.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.methods[0] = edit_first(&l.methods[0], "left");
        r.methods[nm - 1] = edit_last(&r.methods[nm - 1], "right");
        push("edit_two_methods", &l, &r);

        // Genuine conflicts: same line of the same function.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns[a] = edit_first(&l.fns[a], "left");
        r.fns[a] = edit_first(&r.fns[a], "right");
        push("same_line_conflict", &l, &r);

        // Genuine conflicts: delete vs edit of one function.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns.remove(a);
        r.fns[a] = edit_first(&r.fns[a], "right");
        push("delete_vs_edit_same_fn", &l, &r);

        // Genuine conflicts: both add a different function after the last one.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns.push(func("left_new", "", &mut rng));
        r.fns.push(func("right_new", "", &mut rng));
        push("add_fn_both_at_end", &l, &r);

        // One side does not parse: structural must fall back.
        let (mut l, mut r) = (dup(&base), dup(&base));
        l.fns[a] = l.fns[a].replacen("{\n", "{\n    let = ;\n", 1);
        r.fns[b] = edit_first(&r.fns[b], "right");
        push("left_syntax_error", &l, &r);
    }
    out
}
