//! Tiny JSON writer (no serde): results are flat records appended as JSON lines.
pub enum J {
    S(String),
    F(f64),
    U(u64),
    A(Vec<J>),
    O(Vec<(&'static str, J)>),
}
impl J {
    pub fn s(x: impl Into<String>) -> J {
        J::S(x.into())
    }
    pub fn render(&self) -> String {
        match self {
            J::S(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            J::F(f) => {
                if f.is_finite() {
                    format!("{:.6}", f).trim_end_matches('0').trim_end_matches('.').to_string()
                } else {
                    "null".into()
                }
            }
            J::U(u) => u.to_string(),
            J::A(a) => format!("[{}]", a.iter().map(|x| x.render()).collect::<Vec<_>>().join(",")),
            J::O(o) => format!("{{{}}}", o.iter().map(|(k, v)| format!("\"{}\":{}", k, v.render())).collect::<Vec<_>>().join(",")),
        }
    }
}
pub struct Out {
    pub path: String,
    pub dataset: String,
}
impl Out {
    pub fn emit(&self, exp: &str, mut fields: Vec<(&'static str, J)>) {
        fields.insert(0, ("dataset", J::s(self.dataset.clone())));
        fields.insert(0, ("exp", J::s(exp)));
        let line = J::O(fields).render();
        println!("{line}");
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).unwrap();
        writeln!(f, "{line}").unwrap();
    }
}
