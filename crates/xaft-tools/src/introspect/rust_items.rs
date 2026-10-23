//! A small, dependency-free outline of Rust source text.
//!
//! agenthicc's `inspect_agenthicc_source` parses Python with `ast` — "nothing
//! is imported". The Rust equivalent of "nothing is compiled" is a lexer that
//! understands exactly enough of Rust's token grammar to never be fooled by a
//! brace inside a string, a comment or a character literal (raw strings,
//! nested block comments, `'{'` versus a lifetime `'a`), plus an item scanner
//! on top of it that finds `fn`, `struct`, `enum`, `union`, `trait`, `impl`,
//! `mod`, `const`, `static`, `type` and `macro_rules!` items, their line
//! ranges (including their doc comments and attributes, as agenthicc includes
//! decorators), their signatures and doc comments, and the members of every
//! `impl`, `trait`, inline `mod`, `struct` and `enum`.
//!
//! It is an outline, not a parser: expressions are skipped as balanced token
//! groups, and anything it does not recognise is stepped over rather than
//! rejected, so malformed input degrades to fewer items, never to a panic.

/// One item found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The item's name; an `impl` block's is its header (`impl Tool for X`).
    pub name: String,
    /// `function`, `async function`, `struct`, `enum`, `union`, `trait`,
    /// `impl`, `module`, `const`, `static`, `type`, `macro`, `field` or
    /// `variant`.
    pub kind: &'static str,
    /// First line, counting doc comments and attributes (1-based).
    pub start_line: usize,
    /// Last line (1-based).
    pub end_line: usize,
    /// The declaration without its body, whitespace collapsed.
    pub signature: String,
    /// The `///` (or `/** */`) documentation.
    pub docstring: String,
    /// For an `impl` block, the implementing type's name (`X` in
    /// `impl<T> Tool for X<T>`).
    pub self_type: Option<String>,
    /// Items inside an `impl`, `trait` or inline `mod`; fields of a `struct`
    /// or `union`; variants of an `enum`.
    pub children: Vec<Item>,
}

/// A whole file's outline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    /// The file's `//!` documentation.
    pub docstring: String,
    /// Top-level items, in source order.
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Punct(char),
    Open(char),
    Close(char),
    Lit,
    Lifetime,
    OuterDoc(String),
    InnerDoc(String),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    /// Byte offset of the first character.
    start: usize,
    /// Byte offset one past the last character.
    end: usize,
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Strip a doc comment's marker and one following space.
fn doc_text(raw: &str) -> String {
    raw.strip_prefix(' ').unwrap_or(raw).trim_end().to_string()
}

fn lex(src: &str) -> Vec<Token> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let n = chars.len();
    let at = |i: usize| chars.get(i).map(|c| c.1);
    let off = |i: usize| chars.get(i).map_or(src.len(), |c| c.0);
    let mut out = Vec::new();
    let mut i = 0;

    // A quoted body starting at `i` (just after the opening quote): returns
    // the index after the closing quote.
    let quoted = |mut j: usize, quote: char| -> usize {
        while j < n {
            match chars[j].1 {
                '\\' => j += 2,
                c if c == quote => return j + 1,
                _ => j += 1,
            }
        }
        n
    };
    // A raw string: `j` is at the first `#` or the opening `"`.
    let raw = |mut j: usize| -> usize {
        let mut hashes = 0;
        while at(j) == Some('#') {
            hashes += 1;
            j += 1;
        }
        if at(j) != Some('"') {
            return j;
        }
        j += 1;
        while j < n {
            if chars[j].1 == '"' && (1..=hashes).all(|k| at(j + k) == Some('#')) {
                return j + 1 + hashes;
            }
            j += 1;
        }
        n
    };
    let is_raw_start = |j: usize| -> bool {
        // `r"` or `r#…#"` (but `r#ident` is a raw identifier).
        match at(j) {
            Some('"') => true,
            Some('#') => {
                let mut k = j;
                while at(k) == Some('#') {
                    k += 1;
                }
                at(k) == Some('"')
            }
            _ => false,
        }
    };

    while i < n {
        let c = chars[i].1;
        let start = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && at(i + 1) == Some('/') {
            let mut j = i;
            while j < n && chars[j].1 != '\n' {
                j += 1;
            }
            let text = &src[off(i)..off(j)];
            if let Some(rest) = text.strip_prefix("///").filter(|r| !r.starts_with('/')) {
                out.push(Token {
                    tok: Tok::OuterDoc(doc_text(rest)),
                    start: off(i),
                    end: off(j),
                });
            } else if let Some(rest) = text.strip_prefix("//!") {
                out.push(Token {
                    tok: Tok::InnerDoc(doc_text(rest)),
                    start: off(i),
                    end: off(j),
                });
            }
            i = j;
            continue;
        }
        if c == '/' && at(i + 1) == Some('*') {
            let mut depth = 0usize;
            let mut j = i;
            while j < n {
                if chars[j].1 == '/' && at(j + 1) == Some('*') {
                    depth += 1;
                    j += 2;
                } else if chars[j].1 == '*' && at(j + 1) == Some('/') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            let text = &src[off(i)..off(j.min(n))];
            let body = text
                .get(3..text.len().saturating_sub(2))
                .unwrap_or("")
                .lines()
                .map(|l| doc_text(l.trim_start().trim_start_matches('*')))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            if text.starts_with("/**") && !text.starts_with("/***") && text.len() > 4 {
                out.push(Token {
                    tok: Tok::OuterDoc(body),
                    start: off(i),
                    end: off(j),
                });
            } else if text.starts_with("/*!") {
                out.push(Token {
                    tok: Tok::InnerDoc(body),
                    start: off(i),
                    end: off(j),
                });
            }
            i = j;
            continue;
        }
        // String-like literals with prefixes: r"", r#""#, b"", b'', br"", c"", cr"".
        let literal_end = match (c, at(i + 1)) {
            ('r', _) if is_raw_start(i + 1) => Some(raw(i + 1)),
            ('b', Some('"')) | ('c', Some('"')) => Some(quoted(i + 2, '"')),
            ('b', Some('\'')) => Some(quoted(i + 2, '\'')),
            ('b' | 'c', Some('r')) if is_raw_start(i + 2) => Some(raw(i + 2)),
            ('"', _) => Some(quoted(i + 1, '"')),
            _ => None,
        };
        if let Some(j) = literal_end {
            out.push(Token {
                tok: Tok::Lit,
                start: off(start),
                end: off(j),
            });
            i = j;
            continue;
        }
        if c == '\'' {
            // `'\n'` / `'x'` are characters; `'a` / `'static` are lifetimes.
            if at(i + 1) == Some('\\') {
                let j = quoted(i + 1, '\'');
                out.push(Token {
                    tok: Tok::Lit,
                    start: off(i),
                    end: off(j),
                });
                i = j;
            } else if at(i + 2) == Some('\'') {
                out.push(Token {
                    tok: Tok::Lit,
                    start: off(i),
                    end: off(i + 3),
                });
                i += 3;
            } else {
                let mut j = i + 1;
                while at(j).is_some_and(is_ident_char) {
                    j += 1;
                }
                out.push(Token {
                    tok: Tok::Lifetime,
                    start: off(i),
                    end: off(j),
                });
                i = j.max(i + 1);
            }
            continue;
        }
        if is_ident_start(c) {
            // `r#ident` is the identifier `ident`.
            let mut j = i;
            if c == 'r' && at(i + 1) == Some('#') && at(i + 2).is_some_and(is_ident_start) {
                j = i + 2;
            }
            let name_start = j;
            while at(j).is_some_and(is_ident_char) {
                j += 1;
            }
            out.push(Token {
                tok: Tok::Ident(src[off(name_start)..off(j)].to_string()),
                start: off(i),
                end: off(j),
            });
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while at(j).is_some_and(|d| {
                is_ident_char(d) || (d == '.' && at(j + 1).is_some_and(|e| e.is_ascii_digit()))
            }) {
                j += 1;
            }
            out.push(Token {
                tok: Tok::Lit,
                start: off(i),
                end: off(j),
            });
            i = j;
            continue;
        }
        let tok = match c {
            '(' | '[' | '{' => Tok::Open(c),
            ')' | ']' | '}' => Tok::Close(c),
            _ => Tok::Punct(c),
        };
        out.push(Token {
            tok,
            start: off(i),
            end: off(i + 1),
        });
        i += 1;
    }
    out
}

struct Scan<'a> {
    src: &'a str,
    toks: Vec<Token>,
    /// For each `Open`, the index of its `Close` (or the last token).
    close: Vec<usize>,
    line_starts: Vec<usize>,
}

impl<'a> Scan<'a> {
    fn new(src: &'a str) -> Self {
        let toks = lex(src);
        let mut close = vec![0; toks.len()];
        let mut stack: Vec<usize> = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            match t.tok {
                Tok::Open(_) => stack.push(i),
                Tok::Close(_) => {
                    if let Some(o) = stack.pop() {
                        close[o] = i;
                    }
                }
                _ => {}
            }
        }
        let last = toks.len().saturating_sub(1);
        for o in stack {
            close[o] = last;
        }
        let mut line_starts = vec![0];
        line_starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        Self {
            src,
            toks,
            close,
            line_starts,
        }
    }

    fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    fn start_line(&self, i: usize) -> usize {
        self.toks.get(i).map_or(1, |t| self.line_of(t.start))
    }

    fn end_line(&self, i: usize) -> usize {
        self.toks
            .get(i)
            .map_or(1, |t| self.line_of(t.end.saturating_sub(1).max(t.start)))
    }

    fn ident(&self, i: usize) -> Option<&str> {
        match self.toks.get(i).map(|t| &t.tok) {
            Some(Tok::Ident(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    fn is(&self, i: usize, tok: &Tok) -> bool {
        self.toks.get(i).is_some_and(|t| &t.tok == tok)
    }

    fn is_open(&self, i: usize, c: char) -> bool {
        self.is(i, &Tok::Open(c))
    }

    /// Source text of tokens `from..to` (exclusive), whitespace collapsed.
    fn text(&self, from: usize, to: usize) -> String {
        if from >= to || from >= self.toks.len() {
            return String::new();
        }
        let start = self.toks[from].start;
        let end = self.toks[(to - 1).min(self.toks.len() - 1)].end;
        self.src[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// From `i`, find the item's body or terminator without leaving the
    /// current level: `(Some(open_brace), close)` or `(None, semicolon)`.
    fn find_end(&self, mut i: usize, end: usize, want_brace: bool) -> (Option<usize>, usize) {
        while i < end {
            match &self.toks[i].tok {
                Tok::Open('{') if want_brace => return (Some(i), self.close[i]),
                Tok::Open(_) => i = self.close[i] + 1,
                Tok::Punct(';') => return (None, i),
                Tok::Close(_) => return (None, i.saturating_sub(1)),
                _ => i += 1,
            }
        }
        (None, end.saturating_sub(1))
    }

    /// The items between token `i` and `end` (exclusive).
    fn items(&self, mut i: usize, end: usize) -> Vec<Item> {
        let mut items = Vec::new();
        while i < end {
            let mut docs: Vec<String> = Vec::new();
            let mut first: Option<usize> = None;
            // Doc comments and attributes belong to the next item.
            while i < end {
                match &self.toks[i].tok {
                    Tok::OuterDoc(text) => {
                        docs.push(text.clone());
                        first.get_or_insert(i);
                        i += 1;
                    }
                    Tok::InnerDoc(_) => i += 1,
                    Tok::Punct('#')
                        if self.is(i + 1, &Tok::Punct('!')) && self.is_open(i + 2, '[') =>
                    {
                        i = self.close[i + 2] + 1;
                    }
                    Tok::Punct('#') if self.is_open(i + 1, '[') => {
                        first.get_or_insert(i);
                        i = self.close[i + 1] + 1;
                    }
                    _ => break,
                }
            }
            if i >= end {
                break;
            }
            let header = i;
            if self.ident(i) == Some("pub") {
                i += 1;
                if self.is_open(i, '(') {
                    i = self.close[i] + 1;
                }
            }
            let mut is_async = false;
            loop {
                match self.ident(i) {
                    Some("default" | "unsafe" | "safe" | "auto") => i += 1,
                    Some("async") => {
                        is_async = true;
                        i += 1;
                    }
                    Some("const")
                        if matches!(
                            self.ident(i + 1),
                            Some("fn" | "unsafe" | "async" | "extern")
                        ) =>
                    {
                        i += 1
                    }
                    Some("extern") if !matches!(self.ident(i + 1), Some("crate")) => {
                        i += 1;
                        if self.is(i, &Tok::Lit) {
                            i += 1;
                        }
                    }
                    _ => break,
                }
            }
            let start_line = self.start_line(first.unwrap_or(header));
            let docstring = docs.join("\n");
            let make = |name: String, kind: &'static str, sig_end: usize, last: usize| Item {
                name,
                kind,
                start_line,
                end_line: self.end_line(last),
                signature: clip(&self.text(header, sig_end), 400),
                docstring: docstring.clone(),
                self_type: None,
                children: Vec::new(),
            };
            match self.ident(i) {
                Some("fn") => {
                    let name = self.ident(i + 1).unwrap_or("").to_string();
                    let (body, last) = self.find_end(i + 1, end, true);
                    let kind = if is_async {
                        "async function"
                    } else {
                        "function"
                    };
                    items.push(make(name, kind, body.unwrap_or(last), last));
                    i = last + 1;
                }
                Some(kw @ ("struct" | "union" | "enum")) => {
                    let kind: &'static str = match kw {
                        "struct" => "struct",
                        "union" => "union",
                        _ => "enum",
                    };
                    let name = self.ident(i + 1).unwrap_or("").to_string();
                    let (body, last) = self.find_end(i + 1, end, true);
                    let mut item = make(name, kind, body.unwrap_or(last), last);
                    if let Some(open) = body {
                        item.children = self.members(open, kind == "enum");
                    }
                    items.push(item);
                    i = last + 1;
                }
                Some(kw @ ("trait" | "impl" | "mod")) => {
                    let (body, last) = self.find_end(i + 1, end, true);
                    let sig_end = body.unwrap_or(last);
                    let (name, kind) = match kw {
                        "impl" => (self.text(header, sig_end), "impl"),
                        "trait" => (self.ident(i + 1).unwrap_or("").to_string(), "trait"),
                        _ => (self.ident(i + 1).unwrap_or("").to_string(), "module"),
                    };
                    let mut item = make(name, kind, sig_end, last);
                    if kw == "impl" {
                        item.name = clip(&strip_where(&self.text(i, sig_end)), 400);
                        item.self_type = impl_self_type(&self.text(i + 1, sig_end));
                    }
                    if let Some(open) = body {
                        item.children = self.items(open + 1, self.close[open]);
                    }
                    items.push(item);
                    i = last + 1;
                }
                Some(kw @ ("const" | "static" | "type")) => {
                    let kind: &'static str = match kw {
                        "const" => "const",
                        "static" => "static",
                        _ => "type",
                    };
                    let mut n = i + 1;
                    if self.ident(n) == Some("mut") {
                        n += 1;
                    }
                    let name = self.ident(n).unwrap_or("_").to_string();
                    let (_, last) = self.find_end(n, end, false);
                    items.push(make(name, kind, last, last));
                    i = last + 1;
                }
                Some("macro_rules") if self.is(i + 1, &Tok::Punct('!')) => {
                    let name = self.ident(i + 2).unwrap_or("").to_string();
                    let mut last = i + 2;
                    if matches!(self.toks.get(i + 3).map(|t| &t.tok), Some(Tok::Open(_))) {
                        last = self.close[i + 3];
                    }
                    if self.is(last + 1, &Tok::Punct(';')) {
                        last += 1;
                    }
                    items.push(make(name, "macro", i + 3, last));
                    i = last + 1;
                }
                Some("use" | "extern") => {
                    let (_, last) = self.find_end(i + 1, end, false);
                    i = last + 1;
                }
                Some(_) if self.is(i + 1, &Tok::Punct('!')) => {
                    // An item-position macro call (`thread_local! { … }`).
                    let mut last = i + 1;
                    if matches!(self.toks.get(i + 2).map(|t| &t.tok), Some(Tok::Open(_))) {
                        last = self.close[i + 2];
                    }
                    if self.is(last + 1, &Tok::Punct(';')) {
                        last += 1;
                    }
                    i = last + 1;
                }
                _ => {
                    // Not an item: step over it (a group whole).
                    i = match self.toks[i].tok {
                        Tok::Open(_) => self.close[i] + 1,
                        _ => i + 1,
                    };
                }
            }
        }
        items
    }

    /// The fields of a braced `struct`/`union` body, or an `enum`'s variants.
    fn members(&self, open: usize, variants: bool) -> Vec<Item> {
        if !self.is_open(open, '{') {
            return Vec::new();
        }
        let end = self.close[open];
        let mut out = Vec::new();
        let mut i = open + 1;
        while i < end {
            let mut docs = Vec::new();
            let mut first = None;
            while i < end {
                match &self.toks[i].tok {
                    Tok::OuterDoc(text) => {
                        docs.push(text.clone());
                        first.get_or_insert(i);
                        i += 1;
                    }
                    Tok::Punct('#') if self.is_open(i + 1, '[') => {
                        first.get_or_insert(i);
                        i = self.close[i + 1] + 1;
                    }
                    _ => break,
                }
            }
            let header = i;
            // The member runs to the next top-level comma.
            let mut j = i;
            while j < end && !self.is(j, &Tok::Punct(',')) {
                j = match self.toks[j].tok {
                    Tok::Open(_) => self.close[j] + 1,
                    _ => j + 1,
                };
            }
            let mut k = header;
            if self.ident(k) == Some("pub") {
                k += 1;
                if self.is_open(k, '(') {
                    k = self.close[k] + 1;
                }
            }
            if let Some(name) = self.ident(k).filter(|_| k < j) {
                out.push(Item {
                    name: name.to_string(),
                    kind: if variants { "variant" } else { "field" },
                    start_line: self.start_line(first.unwrap_or(header)),
                    end_line: self.end_line(j.saturating_sub(1).max(header)),
                    signature: clip(&self.text(header, j), 400),
                    docstring: docs.join("\n"),
                    self_type: None,
                    children: Vec::new(),
                });
            }
            i = j + 1;
        }
        out
    }
}

/// `text` with any `where` clause removed.
fn strip_where(text: &str) -> String {
    match text.find(" where ") {
        Some(i) => text[..i].to_string(),
        None => text.trim_end_matches(" where").to_string(),
    }
}

/// The implementing type's name from an `impl` header (the text after
/// `impl`): `<T> Tool for Wrapper<T> where …` → `Wrapper`.
fn impl_self_type(header: &str) -> Option<String> {
    let mut rest = strip_where(header).trim().to_string();
    if rest.starts_with('<') {
        // Skip the impl's own generic parameters (`->` is not a bracket).
        let mut depth = 0i32;
        let mut prev = ' ';
        let mut cut = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' if prev != '-' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = i + 1;
                        break;
                    }
                }
                _ => {}
            }
            prev = c;
        }
        rest = rest[cut..].trim().to_string();
    }
    // `Trait for Type` — split at a top-level ` for `.
    let mut depth = 0i32;
    let mut prev = ' ';
    let mut self_ty = rest.as_str();
    for (i, c) in rest.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' if prev != '-' => depth -= 1,
            ')' | ']' => depth -= 1,
            ' ' if depth == 0 && rest[i..].starts_with(" for ") => {
                self_ty = &rest[i + 5..];
                break;
            }
            _ => {}
        }
        prev = c;
    }
    let mut ty = self_ty.trim().trim_start_matches('&').trim();
    if let Some(after) = ty.strip_prefix('\'') {
        // A reference's lifetime: `&'a mut Thing`.
        ty = after.trim_start_matches(is_ident_char).trim();
    }
    let ty = ty
        .trim_start_matches("mut ")
        .trim_start_matches("dyn ")
        .trim();
    let base = ty.split('<').next().unwrap_or(ty);
    let name = base.rsplit("::").next().unwrap_or(base).trim();
    (!name.is_empty() && name.chars().all(is_ident_char)).then(|| name.to_string())
}

/// `text` clipped to `max` characters, marking the cut.
pub fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((cut, _)) => format!("{}…", &text[..cut]),
    }
}

/// Outline `src`.
pub fn outline(src: &str) -> Outline {
    let scan = Scan::new(src);
    // `//!` lines (and the `#![…]` attributes between them) open the file.
    let mut docs = Vec::new();
    let mut i = 0;
    while i < scan.toks.len() {
        match &scan.toks[i].tok {
            Tok::InnerDoc(text) => {
                docs.push(text.clone());
                i += 1;
            }
            Tok::Punct('#') if scan.is(i + 1, &Tok::Punct('!')) && scan.is_open(i + 2, '[') => {
                i = scan.close[i + 2] + 1;
            }
            _ => break,
        }
    }
    let items = scan.items(0, scan.toks.len());
    Outline {
        docstring: docs.join("\n").trim().to_string(),
        items,
    }
}

/// Resolve `path` (`Type`, `Type::method`, `module::Item`, `Enum::Variant`)
/// among `items`.
pub fn find<'a>(items: &'a [Item], path: &[&str]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    if rest.is_empty() {
        return items
            .iter()
            .find(|it| it.kind != "impl" && it.name == *first);
    }
    if let Some(m) = items
        .iter()
        .find(|it| it.kind == "module" && it.name == *first)
    {
        if let Some(found) = find(&m.children, rest) {
            return Some(found);
        }
    }
    for it in items {
        let owner = (it.kind == "impl" && it.self_type.as_deref() == Some(*first))
            || (matches!(it.kind, "trait" | "struct" | "enum" | "union") && it.name == *first);
        if owner {
            if let Some(found) = find(&it.children, rest) {
                return Some(found);
            }
        }
    }
    None
}

/// Every `impl` block in `items` (recursively) whose self type is `name`.
pub fn impls_of<'a>(items: &'a [Item], name: &str) -> Vec<&'a Item> {
    let mut out = Vec::new();
    for it in items {
        if it.kind == "impl" && it.self_type.as_deref() == Some(name) {
            out.push(it);
        }
        if it.kind == "module" {
            out.extend(impls_of(&it.children, name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r##"//! Module docs.
//! Second line.

#![allow(dead_code)]

use std::fmt;

/// The answer.
pub const ANSWER: u32 = { 42 };

/// A tool.
#[derive(Debug, Clone)]
pub struct Widget<T: Clone> {
    /// Its name.
    pub name: String,
    pub(crate) items: Vec<T>,
    tricky: [u8; 3],
}

pub enum Shape {
    /// Round.
    Circle { r: f64 },
    Square(f64),
    Empty,
}

impl<T: Clone> Widget<T> {
    /// Build one.
    pub fn new(name: &str) -> Self {
        let brace = '{';
        let s = "} not a close";
        let r = r#"}"# ;
        /* a } in a /* nested */ comment */
        let _lt: &'static str = "x";
        Self { name: name.into(), items: vec![], tricky: [b'}', 0, 1] }
    }

    async fn fetch(&self) -> Result<(), ()> where T: fmt::Debug {
        Ok(())
    }
}

impl<T: Clone> fmt::Display for Widget<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

pub trait Speak {
    fn speak(&self) -> String;
    fn twice(&self) -> String { self.speak().repeat(2) }
}

macro_rules! shout {
    ($e:expr) => { $e.to_uppercase() };
}

thread_local! { static X: u8 = 0; }

#[cfg(test)]
mod tests {
    #[test]
    fn works() {}
}
"##;

    #[test]
    fn outlines_items_with_lines_signatures_and_docs() {
        let o = outline(SRC);
        assert_eq!(o.docstring, "Module docs.\nSecond line.");
        let names: Vec<(&str, &str)> = o.items.iter().map(|i| (i.name.as_str(), i.kind)).collect();
        assert_eq!(
            names,
            [
                ("ANSWER", "const"),
                ("Widget", "struct"),
                ("Shape", "enum"),
                ("impl<T: Clone> Widget<T>", "impl"),
                ("impl<T: Clone> fmt::Display for Widget<T>", "impl"),
                ("Speak", "trait"),
                ("shout", "macro"),
                ("tests", "module"),
            ]
        );
        let answer = &o.items[0];
        assert_eq!((answer.start_line, answer.end_line), (8, 9));
        assert_eq!(answer.docstring, "The answer.");
        assert_eq!(answer.signature, "pub const ANSWER: u32 = { 42 }");

        let widget = &o.items[1];
        assert_eq!(widget.start_line, 11, "doc comments and attributes count");
        assert_eq!(widget.end_line, 18);
        assert_eq!(widget.signature, "pub struct Widget<T: Clone>");
        let fields: Vec<&str> = widget.children.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fields, ["name", "items", "tricky"]);
        assert_eq!(widget.children[0].docstring, "Its name.");
        assert_eq!(widget.children[2].signature, "tricky: [u8; 3]");

        let variants: Vec<&str> = o.items[2]
            .children
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(variants, ["Circle", "Square", "Empty"]);

        let inherent = &o.items[3];
        assert_eq!(inherent.self_type.as_deref(), Some("Widget"));
        let new = &inherent.children[0];
        assert_eq!(new.name, "new");
        assert_eq!(new.kind, "function");
        assert_eq!(new.docstring, "Build one.");
        assert_eq!(new.signature, "pub fn new(name: &str) -> Self");
        assert_eq!(
            (new.start_line, new.end_line),
            (28, 36),
            "braces in literals and comments are not structure"
        );
        let fetch = &inherent.children[1];
        assert_eq!(fetch.kind, "async function");
        assert_eq!(
            fetch.signature,
            "async fn fetch(&self) -> Result<(), ()> where T: fmt::Debug"
        );
        assert_eq!(o.items[4].self_type.as_deref(), Some("Widget"));

        let speak = &o.items[5];
        let members: Vec<&str> = speak.children.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(members, ["speak", "twice"]);
        assert_eq!(speak.children[0].end_line, speak.children[0].start_line);
        assert_eq!(o.items[7].children[0].name, "works");
    }

    #[test]
    fn symbols_resolve_through_impls_traits_modules_and_variants() {
        let o = outline(SRC);
        assert_eq!(find(&o.items, &["Widget"]).unwrap().kind, "struct");
        assert_eq!(find(&o.items, &["Widget", "new"]).unwrap().name, "new");
        assert!(
            find(&o.items, &["Widget", "fmt"])
                .unwrap()
                .signature
                .contains("Formatter")
        );
        assert_eq!(
            find(&o.items, &["Speak", "twice"]).unwrap().kind,
            "function"
        );
        assert_eq!(
            find(&o.items, &["tests", "works"]).unwrap().kind,
            "function"
        );
        assert_eq!(
            find(&o.items, &["Shape", "Square"]).unwrap().kind,
            "variant"
        );
        assert!(find(&o.items, &["Widget", "nope"]).is_none());
        assert!(find(&o.items, &["Nope"]).is_none());
        assert_eq!(impls_of(&o.items, "Widget").len(), 2);
    }

    #[test]
    fn impl_headers_yield_the_self_type() {
        for (header, want) in [
            ("Foo", Some("Foo")),
            ("<T> Tool for Wrapper<T> where T: Clone", Some("Wrapper")),
            (
                "<F: Fn() -> u8> From<F> for crate::a::Holder<F>",
                Some("Holder"),
            ),
            ("Trait for &'a mut Thing", Some("Thing")),
            ("Trait for &mut Thing", Some("Thing")),
            ("!Send for Raw", Some("Raw")),
        ] {
            assert_eq!(impl_self_type(header).as_deref(), want, "{header}");
        }
    }

    #[test]
    fn hostile_input_degrades_instead_of_panicking() {
        for src in [
            "",
            "fn",
            "impl {",
            "struct S { a: [",
            "'",
            "r#\"unterminated",
            "/* unterminated",
            "fn f() { let c = '\\u{1F600}'; } fn g() {}",
            "pub(in crate::a) fn h<'a>(x: &'a str) -> &'a str { x }",
        ] {
            let _ = outline(src);
        }
        let o = outline("fn f() { let c = '\\u{1F600}'; } fn g() {}");
        let names: Vec<&str> = o.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["f", "g"]);
        let o = outline("pub(in crate::a) fn h<'a>(x: &'a str) -> &'a str { x }");
        assert_eq!(o.items[0].name, "h");
    }
}
