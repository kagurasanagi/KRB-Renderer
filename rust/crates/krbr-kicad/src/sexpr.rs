//! KiCad の S 式（.kicad_pcb など）の最小パーサ

use krbr_core::xxh64;

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub is_list: bool,
    /// アトムが "..." だったか
    pub quoted: bool,
    /// アトムの文字列（リストなら空）
    pub text: String,
    pub items: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: &'static str,
    pub offset: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "S-expression parse error at offset {}: {}", self.offset, self.message)
    }
}

impl Node {
    /// リストの先頭アトム（タグ名）
    pub fn tag(&self) -> &str {
        match self.items.first() {
            Some(first) if self.is_list && !first.is_list => &first.text,
            _ => "",
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 先頭が name の子リスト（最初の 1 つ）
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.items.iter().find(|c| c.is_list && c.tag() == name)
    }

    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.items.iter().filter(move |c| c.is_list && c.tag() == name)
    }

    pub fn has(&self, name: &str) -> bool {
        self.child(name).is_some()
    }

    /// items[i] をアトムとして
    pub fn str(&self, i: usize) -> &str {
        match self.items.get(i) {
            Some(n) if !n.is_list => &n.text,
            _ => "",
        }
    }

    pub fn str_or<'a>(&'a self, i: usize, def: &'a str) -> &'a str {
        match self.items.get(i) {
            Some(n) if !n.is_list => &n.text,
            _ => def,
        }
    }

    pub fn num(&self, i: usize) -> f64 {
        self.num_or(i, 0.0)
    }

    pub fn num_or(&self, i: usize, def: f64) -> f64 {
        match self.items.get(i) {
            Some(n) if !n.is_list => n.text.parse().unwrap_or(def),
            _ => def,
        }
    }

    /// (name v) 形式の子の値
    pub fn child_str(&self, name: &str) -> Option<&str> {
        self.child_str_at(name, 1)
    }

    pub fn child_str_at(&self, name: &str, i: usize) -> Option<&str> {
        let c = self.child(name)?;
        match c.items.get(i) {
            Some(n) if !n.is_list => Some(&n.text),
            _ => None,
        }
    }

    pub fn child_num(&self, name: &str) -> Option<f64> {
        let c = self.child(name)?;
        match c.items.get(1) {
            Some(n) if !n.is_list => Some(c.num(1)),
            _ => None,
        }
    }

    /// (name yes) / (name) / (name no) 形式の真偽値
    pub fn child_bool(&self, name: &str) -> Option<bool> {
        let c = self.child(name)?;
        if c.items.len() < 2 {
            return Some(true);
        }
        let v = c.str(1);
        Some(!(v == "no" || v == "false" || v == "none"))
    }

    /// 位置引数としての素のアトム（例: (pad "1" smd rect ...) の smd）が含まれるか
    pub fn has_atom(&self, atom: &str) -> bool {
        self.items.iter().skip(1).any(|n| !n.is_list && !n.quoted && n.text == atom)
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.s.len() {
            let c = self.s[self.pos];
            if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
                self.pos += 1;
            } else if c == b'#' && (self.pos == 0 || self.s[self.pos - 1] == b'\n') {
                // 行頭コメント（念のため）
                while self.pos < self.s.len() && self.s[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn fail(&self, message: &'static str) -> ParseError {
        ParseError { message, offset: self.pos }
    }

    fn parse_quoted(&mut self) -> Result<Node, ParseError> {
        self.pos += 1; // "
        let mut t: Vec<u8> = Vec::new();
        while self.pos < self.s.len() {
            let c = self.s[self.pos];
            self.pos += 1;
            if c == b'"' {
                return Ok(Node { is_list: false, quoted: true, text: String::from_utf8_lossy(&t).into_owned(), items: Vec::new() });
            }
            if c == b'\\' && self.pos < self.s.len() {
                let e = self.s[self.pos];
                self.pos += 1;
                t.push(match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    other => other,
                });
            } else {
                t.push(c);
            }
        }
        Err(self.fail("unterminated string"))
    }

    fn parse_node(&mut self) -> Result<Node, ParseError> {
        self.skip_ws();
        if self.pos >= self.s.len() {
            return Err(self.fail("unexpected end of input"));
        }
        let c = self.s[self.pos];
        if c == b'(' {
            self.pos += 1;
            let mut out = Node { is_list: true, ..Default::default() };
            loop {
                self.skip_ws();
                if self.pos >= self.s.len() {
                    return Err(self.fail("unterminated list"));
                }
                if self.s[self.pos] == b')' {
                    self.pos += 1;
                    return Ok(out);
                }
                out.items.push(self.parse_node()?);
            }
        }
        if c == b')' {
            return Err(self.fail("unexpected ')'"));
        }
        if c == b'"' {
            return self.parse_quoted();
        }
        let start = self.pos;
        while self.pos < self.s.len() {
            let d = self.s[self.pos];
            if d == b' ' || d == b'\t' || d == b'\r' || d == b'\n' || d == b'(' || d == b')' {
                break;
            }
            self.pos += 1;
        }
        Ok(Node {
            is_list: false,
            quoted: false,
            text: String::from_utf8_lossy(&self.s[start..self.pos]).into_owned(),
            items: Vec::new(),
        })
    }
}

pub fn parse(text: &str) -> Result<Node, ParseError> {
    let mut p = Parser { s: text.as_bytes(), pos: 0 };
    p.parse_node()
}

fn serialize(n: &Node, out: &mut String) {
    if n.is_list {
        out.push('(');
        for c in &n.items {
            serialize(c, out);
            out.push(' ');
        }
        out.push(')');
    } else {
        if n.quoted {
            out.push('"');
        }
        out.push_str(&n.text);
        if n.quoted {
            out.push('"');
        }
    }
}

fn write_node(n: &Node, depth: usize, out: &mut String) {
    if !n.is_list {
        if n.quoted {
            out.push('"');
            for c in n.text.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    c => out.push(c),
                }
            }
            out.push('"');
        } else {
            out.push_str(&n.text);
        }
        return;
    }
    out.push('(');
    for (i, c) in n.items.iter().enumerate() {
        if i > 0 {
            // 最上位の要素は 1 行ずつ（読みやすさのためだけ。KiCad は空白の種類を問わない）
            if depth == 0 && c.is_list {
                out.push_str("\n  ");
            } else {
                out.push(' ');
            }
        }
        write_node(c, depth + 1, out);
    }
    if depth == 0 {
        out.push('\n');
    }
    out.push(')');
}

/// ファイルに書ける S 式にする（文字列は KiCad と同じエスケープ。parse で読み直すと同じ木になる）
pub fn write(n: &Node) -> String {
    let mut s = String::new();
    write_node(n, 0, &mut s);
    s.push('\n');
    s
}

/// ノードの内容（アトムと入れ子の構造）のハッシュ（C++ 版と同じ直列化）
pub fn hash_node(n: &Node) -> u64 {
    let mut s = String::new();
    serialize(n, &mut s);
    xxh64(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_lists_and_strings() {
        let n = parse(r#"(kicad_pcb (version 20260206) (general (thickness 1.6)) (prop "a \"b\"" smd))"#).unwrap();
        assert_eq!(n.tag(), "kicad_pcb");
        assert_eq!(n.child_num("version"), Some(20260206.0));
        assert_eq!(n.child("general").unwrap().child_num("thickness"), Some(1.6));
        let p = n.child("prop").unwrap();
        assert_eq!(p.str(1), "a \"b\"");
        assert!(p.has_atom("smd"));
        assert!(!p.has_atom("a \"b\""));
    }

    #[test]
    fn write_round_trips() {
        // 引用符・逆斜線・改行のエスケープ
        let src = r#"(kicad_pcb (version 1) (gr_text "a \"q\" \\ b\nc" (at 1 2)) (x))"#;
        let n = parse(src).unwrap();
        let back = parse(&write(&n)).unwrap();
        assert_eq!(hash_node(&n), hash_node(&back));
        assert_eq!(back.child("gr_text").unwrap().str(1), "a \"q\" \\ b\nc");
    }

    #[test]
    fn child_bool_forms() {
        let n = parse("(x (hide) (fill yes) (tent no))").unwrap();
        assert_eq!(n.child_bool("hide"), Some(true));
        assert_eq!(n.child_bool("fill"), Some(true));
        assert_eq!(n.child_bool("tent"), Some(false));
        assert_eq!(n.child_bool("none"), None);
    }

    #[test]
    fn errors_carry_offsets() {
        let e = parse("(a (b)").unwrap_err();
        assert_eq!(e.message, "unterminated list");
        assert_eq!(e.offset, 6);
    }

    #[test]
    fn hash_depends_on_content_only() {
        let a = parse("(a  1\n (b \"x\"))").unwrap();
        let b = parse("(a 1 (b \"x\"))").unwrap();
        let c = parse("(a 1 (b x))").unwrap();
        assert_eq!(hash_node(&a), hash_node(&b));
        assert_ne!(hash_node(&a), hash_node(&c));
    }
}
