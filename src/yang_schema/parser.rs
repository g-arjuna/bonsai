//! YANG module parser (RFC 7950 / 7951 subset).
//!
//! This is a real tokenizer + recursive-descent parser — not a regex scan.
//! It produces a typed AST covering modules, submodules, imports, includes,
//! augments, deviations, features, identities, typedefs, groupings, uses,
//! containers, lists, leaves, leaf-lists, choices, cases, anyxml/anydata,
//! rpc/notification shells, and the data-definition substatements needed for
//! schema-aware acquisition (type, units, config, keys, if-feature, etc.).
//!
//! Statements outside the supported grammar are consumed with balanced-token
//! skipping and recorded as diagnostics — parsing never panics on malformed
//! input and never performs unbounded allocation beyond caller-imposed input
//! size limits.

/// Hard bounds enforced by the parser entry point.
pub const MAX_MODULE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MODULE_STATEMENTS: usize = 200_000;
pub const MAX_NESTING_DEPTH: usize = 128;

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
    Eof,
}

fn lex(input: &str) -> Result<Vec<Tok>, String> {
    let b = input.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i] as char;
        match c {
            ' ' | '\t' | '\r' | '\n' => i += 1,
            '/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut closed = false;
                while i + 1 < b.len() {
                    if b[i] == b'*' && b[i + 1] == b'/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Err("unterminated /* comment".into());
                }
            }
            '"' | '\'' => {
                // RFC 7950 §6.1.3: escape processing applies ONLY to
                // double-quoted strings (\" \\ \n \t). Single-quoted strings
                // are literal: backslashes are preserved verbatim and the
                // next quote char always terminates the string.
                let quote = c;
                let qbyte = if quote == '"' { b'"' } else { b'\'' };
                i += 1;
                let mut s = String::new();
                let mut closed = false;
                while i < b.len() {
                    let byte = b[i];
                    if byte == qbyte {
                        i += 1;
                        closed = true;
                        break;
                    }
                    if quote == '"' && byte == b'\\' && i + 1 < b.len() {
                        let n = b[i + 1] as char;
                        match n {
                            'n' => {
                                s.push('\n');
                                i += 2;
                            }
                            't' => {
                                s.push('\t');
                                i += 2;
                            }
                            '"' => {
                                s.push('"');
                                i += 2;
                            }
                            '\\' => {
                                s.push('\\');
                                i += 2;
                            }
                            // Unknown escape: preserve literally, matching
                            // pyang's default (non-strict) behaviour.
                            _ => {
                                s.push('\\');
                                i += 1;
                            }
                        }
                    } else if quote == '"' && (byte == b'\n' || byte == b'\r') {
                        // Multiline double-quoted string: fold the line
                        // break and continuation indentation to a single
                        // space (nothing emitted before a closing quote).
                        if byte == b'\r' && b.get(i + 1) == Some(&b'\n') {
                            i += 1;
                        }
                        i += 1; // consume '\n' (or lone '\r')
                        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
                            i += 1;
                        }
                        if i < b.len() && b[i] != qbyte {
                            s.push(' ');
                        }
                    } else {
                        // Decode the full UTF-8 char; single-quoted strings
                        // keep newlines literally.
                        let ch = input[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if !closed {
                    return Err("unterminated quoted string".into());
                }
                toks.push(Tok::Str(s));
            }
            '{' | '}' | ';' | '(' | ')' | '/' => {
                toks.push(Tok::Punct(c));
                i += 1;
            }
            _ => {
                let start = i;
                while i < b.len() {
                    let ch = b[i] as char;
                    if ch.is_whitespace()
                        || matches!(ch, '{' | '}' | ';' | '(' | ')' | '"' | '\'' | '/')
                    {
                        break;
                    }
                    i += 1;
                }
                if i == start {
                    return Err(format!("unexpected character '{}'", c));
                }
                toks.push(Tok::Ident(input[start..i].to_string()));
            }
        }
    }
    toks.push(Tok::Eof);
    Ok(toks)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub name: String,
    pub is_submodule: bool,
    pub yang_version: String,
    pub namespace: Option<String>,
    pub prefix: Option<String>,
    pub organization: Option<String>,
    pub contact: Option<String>,
    pub description: Option<String>,
    pub revisions: Vec<Revision>,
    pub imports: Vec<Import>,
    pub includes: Vec<String>,
    pub belong_to: Option<String>,
    /// Top-level data-definition / augment / deviation / rpc / notification.
    pub children: Vec<Stmt>,
    pub diagnostics: Vec<String>,
}

impl Module {
    pub fn latest_revision(&self) -> String {
        self.revisions
            .first()
            .map(|r| r.date.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Clone, Debug)]
pub struct Revision {
    pub date: String,
    pub description: Option<String>,
    pub reference: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub module: String,
    pub prefix: Option<String>,
    pub revision: Option<String>,
    pub description: Option<String>,
}

/// A data-definition or module-level statement.
#[derive(Clone, Debug)]
pub enum Stmt {
    Container(Node),
    List(Node),
    Leaf(Node),
    LeafList(Node),
    Choice(Node),
    Case(Node),
    AnyXml(Node),
    AnyData(Node),
    Uses(Uses),
    Augment(Augment),
    Deviation(Deviation),
    Feature(Feature),
    Identity(Identity),
    Typedef(Typedef),
    Grouping(Grouping),
    Rpc(Node),
    Notification(Node),
    Input(Node),
    Output(Node),
    /// Parsed but not interpreted (extension, unknown statement).
    Other {
        keyword: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub description: Option<String>,
    pub reference: Option<String>,
    pub status: Option<String>,
    pub config: Option<bool>,
    pub mandatory: Option<bool>,
    pub presence: Option<String>,
    pub default: Option<String>,
    pub units: Option<String>,
    pub ordered_by: Option<String>,
    pub min_elements: Option<u64>,
    pub max_elements: Option<u64>,
    pub keys: Vec<String>,
    pub musts: Vec<String>,
    pub whens: Vec<String>,
    pub if_features: Vec<String>,
    pub type_spec: Option<TypeSpec>,
    pub children: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Uses {
    pub name: String, // as written, e.g. "oc-if:eth" or "my-grouping"
    pub description: Option<String>,
    pub reference: Option<String>,
    pub status: Option<String>,
    pub whens: Vec<String>,
    pub if_features: Vec<String>,
    pub musts: Vec<String>,
    pub refines: Vec<Refine>,
}

#[derive(Clone, Debug, Default)]
pub struct Refine {
    pub target: String,
    pub mandatory: Option<bool>,
    pub config: Option<bool>,
    pub default: Option<String>,
    pub description: Option<String>,
    pub units: Option<String>,
    pub presence: Option<String>,
    pub min_elements: Option<u64>,
    pub max_elements: Option<u64>,
    /// Keywords inside the refine body that are legal YANG but not
    /// interpreted (uses, augment, must, when, type, if-feature, ...).
    /// Compilation turns these into diagnostics.
    pub unsupported: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Augment {
    pub target: String,
    pub children: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Deviation {
    pub target: String,
    pub description: Option<String>,
    pub reference: Option<String>,
    pub status: Option<String>,
    /// `deviate add|delete|replace|not-supported` statements in this
    /// deviation body. `has_block` records whether a `{ ... }` data-def
    /// block followed the kind (contents are not interpreted).
    pub deviates: Vec<Deviate>,
    pub children: Vec<Deviation>,
}

#[derive(Clone, Debug)]
pub struct Deviate {
    pub kind: String,
    pub has_block: bool,
}

#[derive(Clone, Debug)]
pub struct Feature {
    pub name: String,
    pub description: Option<String>,
    pub reference: Option<String>,
    pub status: Option<String>,
    pub if_features: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub name: String,
    pub base: Option<String>,
    pub description: Option<String>,
    pub reference: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Typedef {
    pub name: String,
    pub type_spec: TypeSpec,
    pub description: Option<String>,
    pub reference: Option<String>,
    pub units: Option<String>,
    pub status: Option<String>,
    pub default: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Grouping {
    pub name: String,
    pub children: Vec<Stmt>,
}

/// A `type` statement: qualified or builtin name plus substatements.
#[derive(Clone, Debug)]
pub struct TypeSpec {
    pub name: String,
    /// Scalar substatements: range/length/pattern/base/path/etc as
    /// (keyword, raw-value) pairs in declaration order.
    pub args: Vec<(String, String)>,
    /// enum members: (label, value)
    pub enums: Vec<(String, String)>,
    /// bit members: (name, position)
    pub bits: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
    statements: usize,
}

pub fn parse_module(input: &str) -> Result<Module, String> {
    if input.len() > MAX_MODULE_BYTES {
        return Err(format!(
            "YANG module exceeds {} byte limit",
            MAX_MODULE_BYTES
        ));
    }
    let toks = lex(input)?;
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
        statements: 0,
    };
    p.parse_module()
}

impl Parser {
    fn peek(&self) -> &Tok {
        self.toks.get(self.pos).unwrap_or(&Tok::Eof)
    }
    fn next(&mut self) -> Tok {
        let t = self.toks.get(self.pos).cloned().unwrap_or(Tok::Eof);
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
        t
    }
    fn at_punct(&self, c: char) -> bool {
        matches!(self.peek(), Tok::Punct(p) if *p == c)
    }
    fn eat_punct(&mut self, c: char) -> bool {
        if self.at_punct(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_punct(&mut self, c: char) -> Result<(), String> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            let lo = self.pos.saturating_sub(10);
            let ctx: Vec<String> = self.toks[lo..self.toks.len().min(self.pos + 2)]
                .iter()
                .map(|t| format!("{:?}", t))
                .collect();
            Err(format!(
                "expected '{}', found {:?} near [{}]",
                c,
                self.peek(),
                ctx.join(" ")
            ))
        }
    }
    fn at_keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s == kw)
    }
    fn eat_keyword(&mut self, kw: &str) -> bool {
        if self.at_keyword(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        match self.next() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("expected identifier, found {:?}", other)),
        }
    }

    /// Identifier-or-quoted-string (YANG allows quoted module/prefix names
    /// and `+`-concatenated string continuations).
    fn ident_or_string(&mut self) -> Result<String, String> {
        match self.next() {
            Tok::Ident(s) => Ok(s),
            Tok::Str(s) => Ok(s),
            other => Err(format!("expected identifier or string, found {:?}", other)),
        }
    }

    /// Node identifier target: a schema-node identifier — bare
    /// `prefix:name`, an absolute `/a/b/c` path, or quoted/`+`-concatenated
    /// string forms used by vendor models. Segments may individually carry
    /// `prefix:` qualifiers; the returned string preserves them verbatim
    /// (including a leading `/`) for namespace-aware resolution.
    fn target_ident(&mut self) -> Result<String, String> {
        let mut out = String::new();
        if self.eat_punct('/') {
            out.push('/');
        }
        // First segment is required.
        match self.peek() {
            Tok::Str(_) | Tok::Ident(_) => {}
            other => {
                return Err(format!("expected target identifier, found {:?}", other));
            }
        }
        loop {
            // One segment: ident/string with optional '+'-joined parts.
            match self.next() {
                Tok::Str(s) => out.push_str(&s),
                Tok::Ident(s) if s == "+" => {
                    return Err("unexpected '+' at start of target segment".into());
                }
                Tok::Ident(s) => out.push_str(&s),
                other => return Err(format!("expected target segment, found {:?}", other)),
            }
            while matches!(self.peek(), Tok::Ident(s) if s == "+") {
                self.pos += 1;
                match self.next() {
                    Tok::Str(s) => out.push_str(&s),
                    Tok::Ident(s) => out.push_str(&s),
                    other => {
                        return Err(format!("expected string after '+', found {:?}", other));
                    }
                }
            }
            if self.eat_punct('/') {
                out.push('/');
                // A trailing '/' with no following segment is malformed.
                match self.peek() {
                    Tok::Str(_) => {}
                    Tok::Ident(s) if s != "+" => {}
                    other => {
                        return Err(format!(
                            "expected target segment after '/', found {:?}",
                            other
                        ));
                    }
                }
                continue;
            }
            break;
        }
        if out.trim().is_empty() || out == "/" {
            return Err("empty target identifier".into());
        }
        Ok(out)
    }
    fn string(&mut self) -> Result<String, String> {
        match self.next() {
            Tok::Str(s) => Ok(s),
            other => Err(format!("expected quoted string, found {:?}", other)),
        }
    }
    fn bump_statement(&mut self) -> Result<(), String> {
        self.statements += 1;
        if self.statements > MAX_MODULE_STATEMENTS {
            return Err("statement count limit exceeded".into());
        }
        Ok(())
    }

    fn parse_module(&mut self) -> Result<Module, String> {
        let mut module = Module {
            yang_version: "1".to_string(),
            ..Default::default()
        };
        if self.eat_keyword("module") {
            module.is_submodule = false;
        } else if self.eat_keyword("submodule") {
            module.is_submodule = true;
        } else {
            return Err(format!(
                "expected 'module' or 'submodule', found {:?}",
                self.peek()
            ));
        }
        module.name = self.ident()?;
        self.expect_punct('{')?;

        loop {
            match self.peek().clone() {
                Tok::Eof => {
                    return Err("unterminated module: unexpected end of input".into());
                }
                Tok::Punct('}') => {
                    self.pos += 1;
                    break;
                }
                Tok::Ident(kw) => {
                    self.bump_statement()?;
                    match kw.as_str() {
                        "yang-version" => {
                            self.pos += 1;
                            module.yang_version = self.ident_or_string()?;
                            self.expect_punct(';')?;
                        }
                        "namespace" => {
                            self.pos += 1;
                            module.namespace = Some(self.string()?);
                            self.expect_punct(';')?;
                        }
                        "prefix" => {
                            self.pos += 1;
                            module.prefix = Some(self.ident_or_string()?);
                            self.expect_punct(';')?;
                        }
                        "organization" | "contact" | "description" | "reference" => {
                            self.pos += 1;
                            let value = self.string_concat()?;
                            self.expect_punct(';')?;
                            match kw.as_str() {
                                "organization" => module.organization = Some(value),
                                "contact" => module.contact = Some(value),
                                "description" => module.description = Some(value),
                                _ => {}
                            }
                        }
                        "revision" => {
                            self.pos += 1;
                            let date = self.ident_or_string()?;
                            let mut rev = Revision {
                                date,
                                description: None,
                                reference: None,
                            };
                            if self.eat_punct('{') {
                                self.depth += 1;
                                self.parse_revision_body(&mut rev)?;
                                self.depth -= 1;
                                self.expect_punct('}')?;
                            } else {
                                self.expect_punct(';')?;
                            }
                            module.revisions.push(rev);
                        }
                        "import" => {
                            self.pos += 1;
                            let mut import = Import {
                                module: self.ident()?,
                                prefix: None,
                                revision: None,
                                description: None,
                            };
                            if self.eat_punct('{') {
                                self.depth += 1;
                                self.parse_import_body(&mut import)?;
                                self.depth -= 1;
                                self.expect_punct('}')?;
                            } else {
                                self.expect_punct(';')?;
                            }
                            module.imports.push(import);
                        }
                        "include" => {
                            self.pos += 1;
                            module.includes.push(self.ident()?);
                            self.expect_punct(';')?;
                        }
                        "belong-to" => {
                            self.pos += 1;
                            module.belong_to = Some(self.ident_or_string()?);
                            self.expect_punct(';')?;
                        }
                        "extension" => {
                            self.pos += 1;
                            self.skip_statement()?;
                            module.children.push(Stmt::Other {
                                keyword: "extension".into(),
                            });
                        }
                        "feature" => {
                            self.pos += 1;
                            let f = self.parse_feature()?;
                            module.children.push(Stmt::Feature(f));
                        }
                        "identity" => {
                            self.pos += 1;
                            let i = self.parse_identity()?;
                            module.children.push(Stmt::Identity(i));
                        }
                        "typedef" => {
                            self.pos += 1;
                            let t = self.parse_typedef()?;
                            module.children.push(Stmt::Typedef(t));
                        }
                        "grouping" => {
                            self.pos += 1;
                            let g = self.parse_grouping()?;
                            module.children.push(Stmt::Grouping(g));
                        }
                        "augment" => {
                            self.pos += 1;
                            let a = self.parse_augment()?;
                            module.children.push(Stmt::Augment(a));
                        }
                        "deviation" => {
                            self.pos += 1;
                            let d = self.parse_deviation()?;
                            module.children.push(Stmt::Deviation(d));
                        }
                        "rpc" => {
                            self.pos += 1;
                            let n = self.parse_node()?;
                            module.children.push(Stmt::Rpc(n));
                        }
                        "notification" => {
                            self.pos += 1;
                            let n = self.parse_node()?;
                            module.children.push(Stmt::Notification(n));
                        }
                        "container" | "list" | "leaf" | "leaf-list" | "choice" | "case"
                        | "anyxml" | "anydata" | "input" | "output" => {
                            self.pos += 1;
                            if let Some(stmt) = self.parse_node_dispatch(&kw)? {
                                module.children.push(stmt);
                            }
                        }
                        "uses" => {
                            self.pos += 1;
                            let name = self.ident()?;
                            module
                                .children
                                .push(Stmt::Uses(self.parse_uses_body(name)?));
                        }
                        _ => {
                            self.pos += 1;
                            self.skip_rest_of_statement()?;
                            // `prefix:name;` at module level is an extension
                            // instance (e.g. oc-ext:catalog-organization) —
                            // legal YANG, not an unsupported construct.
                            if !kw.contains(':') {
                                module.diagnostics.push(format!(
                                    "skipped unsupported top-level statement '{}' in module {}",
                                    kw, module.name
                                ));
                            }
                        }
                    }
                }
                _ => {
                    self.pos += 1;
                    module
                        .diagnostics
                        .push(format!("unexpected token {:?} in module", self.peek()));
                }
            }
        }
        // Revisions are conventionally newest-first; sort descending for
        // deterministic latest_revision().
        module.revisions.sort_by(|a, b| b.date.cmp(&a.date));
        if !matches!(self.peek(), Tok::Eof) {
            module.diagnostics.push(format!(
                "unexpected token {:?} after module '{}' closing brace",
                self.peek(),
                module.name
            ));
        }
        Ok(module)
    }

    fn parse_revision_body(&mut self, rev: &mut Revision) -> Result<(), String> {
        loop {
            if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                return Ok(());
            }
            if self.eat_keyword("description") {
                rev.description = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("reference") {
                rev.reference = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else {
                self.skip_statement()?;
            }
        }
    }

    fn parse_import_body(&mut self, import: &mut Import) -> Result<(), String> {
        loop {
            if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                return Ok(());
            }
            if self.eat_keyword("prefix") {
                import.prefix = Some(self.ident_or_string()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("revision") {
                import.revision = Some(self.ident_or_string()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("description") {
                import.description = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("reference") {
                self.string_concat()?;
                self.expect_punct(';')?;
            } else {
                self.skip_statement()?;
            }
        }
    }

    fn string_concat(&mut self) -> Result<String, String> {
        let mut out = String::new();
        let mut seen = false;
        loop {
            match self.peek() {
                Tok::Str(s) => {
                    seen = true;
                    out.push_str(s);
                    self.pos += 1;
                }
                // YANG allows "..." + "..." concatenation.
                Tok::Ident(s) if s == "+" && seen => {
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if !seen {
            return Err(format!("expected string, found {:?}", self.peek()));
        }
        Ok(out)
    }

    fn parse_feature(&mut self) -> Result<Feature, String> {
        let name = self.ident()?;
        let mut f = Feature {
            name,
            description: None,
            reference: None,
            status: None,
            if_features: Vec::new(),
        };
        // `feature name;` (bodiless) is legal YANG — body is optional.
        if self.eat_punct('{') {
            self.depth += 1;
            if self.depth > MAX_NESTING_DEPTH {
                return Err("maximum nesting depth exceeded".into());
            }
            self.parse_feature_body(&mut f)?;
            self.depth -= 1;
            self.expect_punct('}')?;
        } else {
            self.expect_punct(';')?;
        }
        Ok(f)
    }

    fn parse_feature_body(&mut self, f: &mut Feature) -> Result<(), String> {
        loop {
            if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                return Ok(());
            }
            if self.eat_keyword("description") {
                f.description = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("reference") {
                f.reference = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("status") {
                f.status = Some(self.ident()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("if-feature") {
                f.if_features.push(self.ident_or_string()?);
                self.eat_punct(';');
            } else {
                self.skip_statement()?;
            }
        }
    }

    fn parse_identity(&mut self) -> Result<Identity, String> {
        let name = self.ident()?;
        let mut i = Identity {
            name,
            base: None,
            description: None,
            reference: None,
            status: None,
        };
        self.expect_punct('{')?;
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err("maximum nesting depth exceeded".into());
        }
        loop {
            if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                break;
            }
            if self.eat_keyword("base") {
                i.base = Some(self.ident()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("description") {
                i.description = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("reference") {
                i.reference = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("status") {
                i.status = Some(self.ident()?);
                self.expect_punct(';')?;
            } else {
                self.skip_statement()?;
            }
        }
        self.depth -= 1;
        self.expect_punct('}')?;
        Ok(i)
    }

    fn parse_typedef(&mut self) -> Result<Typedef, String> {
        let name = self.ident()?;
        let mut t = Typedef {
            name,
            type_spec: TypeSpec {
                name: String::new(),
                args: Vec::new(),
                enums: Vec::new(),
                bits: Vec::new(),
            },
            description: None,
            reference: None,
            units: None,
            status: None,
            default: None,
        };
        self.expect_punct('{')?;
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err("maximum nesting depth exceeded".into());
        }
        loop {
            if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                break;
            }
            if self.eat_keyword("type") {
                t.type_spec = self.parse_type_spec()?;
            } else if self.eat_keyword("description") {
                t.description = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("reference") {
                t.reference = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("units") {
                t.units = Some(self.ident_or_string()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("status") {
                t.status = Some(self.ident()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("default") {
                t.default = Some(self.raw_value_until_semi()?);
            } else {
                self.skip_statement()?;
            }
        }
        self.depth -= 1;
        self.expect_punct('}')?;
        Ok(t)
    }

    fn parse_grouping(&mut self) -> Result<Grouping, String> {
        let name = self.ident()?;
        self.expect_punct('{')?;
        let children = self.parse_stmt_list()?;
        self.expect_punct('}')?;
        Ok(Grouping { name, children })
    }

    fn parse_augment(&mut self) -> Result<Augment, String> {
        let target = self.target_ident()?;
        self.expect_punct('{')?;
        let children = self.parse_stmt_list()?;
        self.expect_punct('}')?;
        Ok(Augment { target, children })
    }

    fn parse_deviation(&mut self) -> Result<Deviation, String> {
        let target = self.target_ident()?;
        self.expect_punct('{')?;
        let mut d = Deviation {
            target,
            description: None,
            reference: None,
            status: None,
            deviates: Vec::new(),
            children: Vec::new(),
        };
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err("maximum nesting depth exceeded".into());
        }
        loop {
            if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                break;
            }
            if self.eat_keyword("description") {
                d.description = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("reference") {
                d.reference = Some(self.string_concat()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("status") {
                d.status = Some(self.ident()?);
                self.expect_punct(';')?;
            } else if self.eat_keyword("deviation") {
                let child = self.parse_deviation()?;
                d.children.push(child);
            } else if self.eat_keyword("deviate") {
                d.deviates.push(self.parse_deviate()?);
            } else {
                self.skip_statement()?;
            }
        }
        self.depth -= 1;
        self.expect_punct('}')?;
        Ok(d)
    }

    /// `deviate <kind> [ { data-defs } ] ;` inside a deviation body.
    /// The kind is recorded; block contents are consumed but not
    /// interpreted (add/delete/replace semantics are partial — see
    /// docs/YANG_CATALOGUE.md).
    fn parse_deviate(&mut self) -> Result<Deviate, String> {
        let kind = self.ident()?;
        let has_block = self.eat_punct('{');
        if has_block {
            self.depth += 1;
            if self.depth > MAX_NESTING_DEPTH {
                return Err("maximum nesting depth exceeded".into());
            }
            self.skip_balanced()?;
            self.depth -= 1;
        }
        // Both forms end with ';' — `deviate kind;` and `deviate kind {…};`.
        self.expect_punct(';')?;
        Ok(Deviate { kind, has_block })
    }

    /// Parse statements until '}' or EOF. Does not consume the closing '}'.
    /// Guarded against unbounded recursion via grouping/augment nesting.
    fn parse_stmt_list(&mut self) -> Result<Vec<Stmt>, String> {
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err("maximum nesting depth exceeded".into());
        }
        let mut out = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::Punct('}') | Tok::Eof => break,
                Tok::Ident(kw) => {
                    self.bump_statement()?;
                    match kw.as_str() {
                        "uses" => {
                            self.pos += 1;
                            let name = self.ident()?;
                            out.push(Stmt::Uses(self.parse_uses_body(name)?));
                        }
                        "augment" => {
                            self.pos += 1;
                            out.push(Stmt::Augment(self.parse_augment()?));
                        }
                        "deviation" => {
                            self.pos += 1;
                            out.push(Stmt::Deviation(self.parse_deviation()?));
                        }
                        "feature" => {
                            self.pos += 1;
                            out.push(Stmt::Feature(self.parse_feature()?));
                        }
                        "identity" => {
                            self.pos += 1;
                            out.push(Stmt::Identity(self.parse_identity()?));
                        }
                        "typedef" => {
                            self.pos += 1;
                            out.push(Stmt::Typedef(self.parse_typedef()?));
                        }
                        "grouping" => {
                            self.pos += 1;
                            out.push(Stmt::Grouping(self.parse_grouping()?));
                        }
                        "rpc" => {
                            self.pos += 1;
                            out.push(Stmt::Rpc(self.parse_node()?));
                        }
                        "notification" => {
                            self.pos += 1;
                            out.push(Stmt::Notification(self.parse_node()?));
                        }
                        "container" | "list" | "leaf" | "leaf-list" | "choice" | "case"
                        | "anyxml" | "anydata" | "input" | "output" => {
                            self.pos += 1;
                            if let Some(stmt) = self.parse_node_dispatch(&kw)? {
                                out.push(stmt);
                            }
                        }
                        _ => {
                            // Property/must/when/type/etc statements belong to
                            // the enclosing node body; parse_node_body handles
                            // them. Here they are unexpected — skip safely.
                            self.pos += 1;
                            self.skip_rest_of_statement()?;
                        }
                    }
                }
                _ => {
                    self.pos += 1;
                    self.skip_rest_of_statement()?;
                }
            }
        }
        self.depth -= 1;
        Ok(out)
    }

    /// Parse `name { node-body }` for a data-definition keyword already
    /// consumed. Returns None for keywords that turned out unsupported.
    fn parse_node_dispatch(&mut self, keyword: &str) -> Result<Option<Stmt>, String> {
        let stmt = match keyword {
            "container" => Some(Stmt::Container(self.parse_node()?)),
            "list" => Some(Stmt::List(self.parse_node()?)),
            "leaf" => Some(Stmt::Leaf(self.parse_node()?)),
            "leaf-list" => Some(Stmt::LeafList(self.parse_node()?)),
            "choice" => Some(Stmt::Choice(self.parse_node()?)),
            "case" => Some(Stmt::Case(self.parse_node()?)),
            "anyxml" => Some(Stmt::AnyXml(self.parse_node()?)),
            "anydata" => Some(Stmt::AnyData(self.parse_node()?)),
            "input" => Some(Stmt::Input(self.parse_node()?)),
            "output" => Some(Stmt::Output(self.parse_node()?)),
            _ => None,
        };
        Ok(stmt)
    }

    /// Parse a full node: optional name, '{', mixed body (properties +
    /// child data-defs), '}'.
    fn parse_node(&mut self) -> Result<Node, String> {
        let name = match self.peek() {
            Tok::Ident(_) => self.ident()?,
            Tok::Punct('{') => String::new(),
            other => return Err(format!("expected node name, found {:?}", other)),
        };
        self.expect_punct('{')?;
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err("maximum nesting depth exceeded".into());
        }
        let mut node = Node {
            name,
            ..Node::default()
        };
        self.parse_node_body(&mut node)?;
        self.depth -= 1;
        self.expect_punct('}')?;
        Ok(node)
    }

    /// Parse the interior of a node body: property statements and nested
    /// data-definition statements intermixed.
    fn parse_node_body(&mut self, node: &mut Node) -> Result<(), String> {
        loop {
            match self.peek().clone() {
                Tok::Punct('}') | Tok::Eof => return Ok(()),
                Tok::Ident(kw) => {
                    self.bump_statement()?;
                    match kw.as_str() {
                        // Nested data-definitions
                        "container" | "list" | "leaf" | "leaf-list" | "choice" | "case"
                        | "anyxml" | "anydata" | "input" | "output" => {
                            self.pos += 1;
                            if let Some(stmt) = self.parse_node_dispatch(&kw)? {
                                node.children.push(stmt);
                            }
                        }
                        "uses" => {
                            self.pos += 1;
                            let name = self.ident()?;
                            node.children.push(Stmt::Uses(self.parse_uses_body(name)?));
                        }
                        "augment" => {
                            self.pos += 1;
                            node.children.push(Stmt::Augment(self.parse_augment()?));
                        }
                        "deviation" => {
                            self.pos += 1;
                            node.children.push(Stmt::Deviation(self.parse_deviation()?));
                        }
                        // Properties
                        "type" => {
                            self.pos += 1;
                            node.type_spec = Some(self.parse_type_spec()?);
                        }
                        "units" => {
                            self.pos += 1;
                            node.units = Some(self.ident_or_string()?);
                            self.expect_punct(';')?;
                        }
                        "config" => {
                            self.pos += 1;
                            node.config = Some(self.bool_value()?);
                            self.expect_punct(';')?;
                        }
                        "mandatory" => {
                            self.pos += 1;
                            node.mandatory = Some(self.bool_value()?);
                            self.expect_punct(';')?;
                        }
                        "default" => {
                            self.pos += 1;
                            node.default = Some(self.raw_value_until_semi()?);
                        }
                        "description" => {
                            self.pos += 1;
                            node.description = Some(self.string_concat()?);
                            self.expect_punct(';')?;
                        }
                        "reference" => {
                            self.pos += 1;
                            node.reference = Some(self.string_concat()?);
                            self.expect_punct(';')?;
                        }
                        "status" => {
                            self.pos += 1;
                            node.status = Some(self.ident()?);
                            self.expect_punct(';')?;
                        }
                        "min-elements" => {
                            self.pos += 1;
                            node.min_elements = Some(self.uint_value()?);
                            self.expect_punct(';')?;
                        }
                        "max-elements" => {
                            self.pos += 1;
                            node.max_elements = Some(self.uint_value()?);
                            self.expect_punct(';')?;
                        }
                        "key" => {
                            self.pos += 1;
                            // key-stmt argument is a space-separated list of
                            // key leaf names in declaration order; RFC 7950
                            // quotes it, but tolerate the unquoted form.
                            let arg = match self.peek() {
                                Tok::Str(_) => {
                                    let s = self.string()?;
                                    self.expect_punct(';')?;
                                    s
                                }
                                _ => self.raw_value_until_semi()?,
                            };
                            for k in arg.split_whitespace() {
                                node.keys.push(k.to_string());
                            }
                        }
                        "presence" => {
                            self.pos += 1;
                            node.presence = Some(self.ident()?);
                            self.expect_punct(';')?;
                        }
                        "ordered-by" => {
                            self.pos += 1;
                            node.ordered_by = Some(self.ident()?);
                            // optional '(' ... ')' — tolerate
                            if self.eat_punct('(') {
                                self.skip_until(')');
                                self.eat_punct(')');
                            }
                            self.expect_punct(';')?;
                        }
                        "must" => {
                            self.pos += 1;
                            node.musts.push(self.string_concat()?);
                            if self.eat_punct('{') {
                                self.skip_balanced()?;
                            } else {
                                self.expect_punct(';')?;
                            }
                        }
                        "when" => {
                            self.pos += 1;
                            node.whens.push(self.string_concat()?);
                            if self.eat_punct('{') {
                                self.skip_balanced()?;
                            } else {
                                self.expect_punct(';')?;
                            }
                        }
                        "if-feature" => {
                            self.pos += 1;
                            node.if_features.push(self.ident_or_string()?);
                            self.eat_punct(';');
                        }
                        _ => {
                            self.pos += 1;
                            self.skip_rest_of_statement()?;
                        }
                    }
                }
                _ => {
                    self.pos += 1;
                    self.skip_rest_of_statement()?;
                }
            }
        }
    }

    fn bool_value(&mut self) -> Result<bool, String> {
        match self.next() {
            Tok::Ident(s) if s == "true" => Ok(true),
            Tok::Ident(s) if s == "false" => Ok(false),
            other => Err(format!("expected true/false, found {:?}", other)),
        }
    }

    fn uint_value(&mut self) -> Result<u64, String> {
        match self.next() {
            Tok::Ident(s) => s
                .parse::<u64>()
                .map_err(|_| format!("expected unsigned integer, found '{}'", s)),
            Tok::Str(s) => s
                .parse::<u64>()
                .map_err(|_| format!("expected unsigned integer, found '{}'", s)),
            other => Err(format!("expected unsigned integer, found {:?}", other)),
        }
    }

    /// Consume tokens until ';' at the current brace depth and return their
    /// joined text (default values, leafref paths, etc.).
    fn raw_value_until_semi(&mut self) -> Result<String, String> {
        let mut parts = Vec::new();
        let mut depth = 0i32;
        loop {
            match self.peek().clone() {
                Tok::Eof => return Err("unterminated value (no ';')".into()),
                Tok::Punct('{') => {
                    depth += 1;
                    parts.push("{".to_string());
                    self.pos += 1;
                }
                Tok::Punct('}') => {
                    if depth == 0 {
                        return Err("unterminated value (unexpected '}')".into());
                    }
                    depth -= 1;
                    parts.push("}".to_string());
                    self.pos += 1;
                }
                Tok::Punct(';') if depth == 0 => {
                    self.pos += 1;
                    return Ok(parts.join(" ").trim().to_string());
                }
                Tok::Ident(s) => {
                    parts.push(s);
                    self.pos += 1;
                }
                Tok::Str(s) => {
                    parts.push(format!("\"{}\"", s));
                    self.pos += 1;
                }
                Tok::Punct(c) => {
                    parts.push(c.to_string());
                    self.pos += 1;
                }
            }
        }
    }

    /// Parse `type <name> [ { type-substmts } ] ;`
    fn parse_type_spec(&mut self) -> Result<TypeSpec, String> {
        let name = match self.peek() {
            Tok::Ident(_) => self.ident()?,
            other => return Err(format!("expected type name, found {:?}", other)),
        };
        let mut spec = TypeSpec {
            name,
            args: Vec::new(),
            enums: Vec::new(),
            bits: Vec::new(),
        };
        let braced = self.eat_punct('{');
        if braced {
            self.depth += 1;
            if self.depth > MAX_NESTING_DEPTH {
                return Err("maximum nesting depth exceeded".into());
            }
            loop {
                if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                    break;
                }
                match self.peek().clone() {
                    Tok::Ident(kw) => {
                        self.pos += 1;
                        match kw.as_str() {
                            "range" | "length" | "pattern" | "base" | "path"
                            | "require-instance" | "fraction-digits" | "default" | "mandatory"
                            | "units" | "status" | "description" | "reference" => {
                                let value = self.raw_value_until_semi()?;
                                spec.args.push((kw, value));
                            }
                            "enum" => {
                                // enum <name> { value N; description ...; }
                                let mut ename = String::new();
                                if matches!(self.peek(), Tok::Ident(_) | Tok::Str(_)) {
                                    ename = match self.next() {
                                        Tok::Ident(s) | Tok::Str(s) => s,
                                        _ => unreachable!(),
                                    };
                                }
                                self.expect_punct('{')?;
                                loop {
                                    if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                                        break;
                                    }
                                    if self.eat_keyword("value") {
                                        // enum integer value — metadata only
                                        self.skip_rest_of_statement()?;
                                    } else {
                                        self.skip_statement()?;
                                    }
                                }
                                self.expect_punct('}')?;
                                spec.enums.push((ename.clone(), ename));
                            }
                            "bit" => {
                                // bit "name" { position N; }
                                let bit_name = match self.next() {
                                    Tok::Str(s) => s,
                                    Tok::Ident(s) => s,
                                    other => {
                                        return Err(format!(
                                            "expected bit name, found {:?}",
                                            other
                                        ));
                                    }
                                };
                                let mut position = String::new();
                                if self.eat_punct('{') {
                                    self.depth += 1;
                                    loop {
                                        if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                                            break;
                                        }
                                        if self.eat_keyword("position") {
                                            position = self.raw_value_until_semi()?;
                                        } else {
                                            self.skip_statement()?;
                                        }
                                    }
                                    self.depth -= 1;
                                    self.expect_punct('}')?;
                                } else {
                                    self.expect_punct(';')?;
                                }
                                spec.bits.push((bit_name, position));
                            }
                            _ => {
                                self.skip_rest_of_statement()?;
                            }
                        }
                    }
                    _ => {
                        self.pos += 1;
                        self.skip_rest_of_statement()?;
                    }
                }
            }
            self.depth -= 1;
            self.expect_punct('}')?;
        }
        if !braced {
            self.expect_punct(';')?;
        }
        Ok(spec)
    }

    fn parse_uses_body(&mut self, name: String) -> Result<Uses, String> {
        let mut uses = Uses {
            name,
            description: None,
            reference: None,
            status: None,
            whens: Vec::new(),
            if_features: Vec::new(),
            musts: Vec::new(),
            refines: Vec::new(),
        };
        if !self.eat_punct('{') {
            self.expect_punct(';')?;
            return Ok(uses);
        }
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err("maximum nesting depth exceeded".into());
        }
        loop {
            if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                break;
            }
            match self.peek().clone() {
                Tok::Ident(kw) => {
                    self.pos += 1;
                    match kw.as_str() {
                        "refine" => {
                            let target = self.target_ident()?;
                            self.expect_punct('{')?;
                            let mut r = Refine {
                                target,
                                ..Refine::default()
                            };
                            loop {
                                if self.at_punct('}') || matches!(self.peek(), Tok::Eof) {
                                    break;
                                }
                                if self.eat_keyword("mandatory") {
                                    r.mandatory = Some(self.bool_value()?);
                                    self.expect_punct(';')?;
                                } else if self.eat_keyword("config") {
                                    r.config = Some(self.bool_value()?);
                                    self.expect_punct(';')?;
                                } else if self.eat_keyword("default") {
                                    r.default = Some(self.raw_value_until_semi()?);
                                } else if self.eat_keyword("description") {
                                    r.description = Some(self.string_concat()?);
                                    self.expect_punct(';')?;
                                } else if self.eat_keyword("units") {
                                    r.units = Some(self.ident_or_string()?);
                                    self.expect_punct(';')?;
                                } else if self.eat_keyword("presence") {
                                    r.presence = Some(self.ident()?);
                                    self.expect_punct(';')?;
                                } else if self.eat_keyword("min-elements") {
                                    r.min_elements = Some(self.uint_value()?);
                                    self.expect_punct(';')?;
                                } else if self.eat_keyword("max-elements") {
                                    r.max_elements = Some(self.uint_value()?);
                                    self.expect_punct(';')?;
                                } else {
                                    // refine may legally contain uses,
                                    // augment, must/when, type, if-feature
                                    // etc. — record the ACTUAL keyword so
                                    // compilation can diagnose rather than
                                    // silently drop.
                                    let bad = match self.peek() {
                                        Tok::Ident(s) => s.clone(),
                                        other => format!("{:?}", other),
                                    };
                                    r.unsupported.push(bad);
                                    self.skip_statement()?;
                                }
                            }
                            self.expect_punct('}')?;
                            uses.refines.push(r);
                        }
                        "description" => {
                            uses.description = Some(self.string_concat()?);
                            self.expect_punct(';')?;
                        }
                        "reference" => {
                            uses.reference = Some(self.string_concat()?);
                            self.expect_punct(';')?;
                        }
                        "status" => {
                            uses.status = Some(self.ident()?);
                            self.expect_punct(';')?;
                        }
                        "when" => {
                            uses.whens.push(self.string_concat()?);
                            if self.eat_punct('{') {
                                self.skip_balanced()?;
                            } else {
                                self.expect_punct(';')?;
                            }
                        }
                        "if-feature" => {
                            uses.if_features.push(self.ident_or_string()?);
                            self.eat_punct(';');
                        }
                        "must" => {
                            uses.musts.push(self.string_concat()?);
                            if self.eat_punct('{') {
                                self.skip_balanced()?;
                            } else {
                                self.expect_punct(';')?;
                            }
                        }
                        _ => {
                            self.pos += 1;
                            self.skip_rest_of_statement()?;
                        }
                    }
                }
                _ => {
                    self.pos += 1;
                    self.skip_rest_of_statement()?;
                }
            }
        }
        self.depth -= 1;
        self.expect_punct('}')?;
        Ok(uses)
    }

    /// Skip tokens until the next ';' at brace-depth zero relative to now.
    /// Used for statements whose keyword was already consumed.
    fn skip_rest_of_statement(&mut self) -> Result<(), String> {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Tok::Eof => return Ok(()),
                Tok::Punct('{') => {
                    depth += 1;
                    self.pos += 1;
                }
                Tok::Punct('}') => {
                    if depth == 0 {
                        return Ok(()); // leave '}' for caller
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                Tok::Punct(';') if depth == 0 => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Skip a complete statement including a balanced {...} group.
    fn skip_statement(&mut self) -> Result<(), String> {
        self.skip_rest_of_statement()
    }

    /// Consume a balanced {...} block (caller already ate '{').
    fn skip_balanced(&mut self) -> Result<(), String> {
        let mut depth = 1i32;
        while depth > 0 {
            match self.next() {
                Tok::Punct('{') => depth += 1,
                Tok::Punct('}') => depth -= 1,
                Tok::Eof => return Err("unterminated block".into()),
                _ => {}
            }
        }
        Ok(())
    }

    fn skip_until(&mut self, c: char) {
        loop {
            let done = match self.peek() {
                Tok::Eof => true,
                Tok::Punct(p) => *p == c,
                _ => false,
            };
            if done {
                break;
            }
            self.pos += 1;
        }
    }
}
