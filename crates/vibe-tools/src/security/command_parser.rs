//! A conservative POSIX shell command-line parser.
//!
//! The parser does not execute or expand anything. Its only job is to find
//! every program a command line would start, together with the context the
//! security policy needs, so that each of them can be checked. It therefore:
//!
//! - splits on `|`, `||`, `|&`, `&&`, `;`, `&` and newlines, remembering which
//!   commands form a pipeline;
//! - recursively extracts the commands inside `$( … )`, backticks, subshells
//!   `( … )` and process substitutions `<( … )` / `>( … )`;
//! - records redirections (`>`, `2>&1`, `<<EOF` …) with their targets and
//!   skips here-document bodies;
//! - records leading environment assignments (`FOO=bar cmd`, `env FOO=bar
//!   cmd`) and loop variables (`for NAME in …`);
//! - strips wrapper programs (`env`, `time`, `nice`, `nohup`, `exec`,
//!   `command`, `builtin`, `timeout`, `xargs`, `setsid`, `stdbuf`,
//!   `caffeinate`, `chronic`, `unbuffer`, `ionice`, `taskset`, `flock`,
//!   `watch`, `strace`, `ltrace`, `script`, `busybox`, `toybox`) and parses the
//!   command strings some of them take (`flock -c`, `watch`, `script -c`);
//! - removes quotes and backslash escapes from every word.
//!
//! Anything it does not understand (unbalanced quotes or parentheses, `case`
//! statements, `env -S`, `xargs` without an explicit program, …) is reported
//! as a [`ParseError`]. Callers must treat an error as a denial.

/// Maximum nesting of command substitutions, subshells and `sh -c` strings.
pub const MAX_NESTING_DEPTH: usize = 16;

/// A redirection attached to a command, such as `> out.txt` or `2>&1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirection {
    /// The operator without its file-descriptor prefix: `>`, `>>`, `>|`,
    /// `>&`, `&>`, `&>>`, `<`, `<>`, `<&`, `<<`, `<<-` or `<<<`.
    pub operator: String,
    /// The target word after quote removal (a file, a descriptor number or a
    /// here-document delimiter).
    pub target: String,
}

impl Redirection {
    /// Whether this redirection creates or writes a file.
    #[must_use]
    pub fn writes_file(&self) -> bool {
        match self.operator.as_str() {
            ">" | ">>" | ">|" | "&>" | "&>>" | "<>" => true,
            ">&" => !(self.target == "-" || self.target.chars().all(|c| c.is_ascii_digit())),
            _ => false,
        }
    }
}

/// One simple command found in a command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSegment {
    /// Normalised program name: the last path component of the first word,
    /// lowercased, with a trailing `.exe` removed (`/usr/bin/Git.exe` → `git`).
    /// Empty when the command only assigns variables or redirects.
    pub program: String,
    /// The words of the command after quote removal, starting with the
    /// program exactly as written. Empty when `program` is empty.
    pub argv: Vec<String>,
    /// Environment or shell variables set by the command, as `(name, value)`:
    /// leading `NAME=value` words, `env NAME=value` arguments and `for NAME`
    /// loop variables.
    pub assignments: Vec<(String, String)>,
    /// Redirections of the command.
    pub redirections: Vec<Redirection>,
    /// Wrapper programs stripped in front of the program, outermost first
    /// (normalised like `program`).
    pub wrappers: Vec<String>,
    /// Identifier of the pipeline the command belongs to. Commands joined by
    /// `|` share it; identifiers are unique within one parse result.
    pub pipeline: usize,
    /// Whether the command's standard input comes from a previous command of
    /// the same pipeline.
    pub piped_input: bool,
}

impl CommandSegment {
    /// Arguments after the program word.
    #[must_use]
    pub fn args(&self) -> &[String] {
        self.argv.get(1..).unwrap_or(&[])
    }

    /// Whether the segment runs a program (as opposed to only assigning
    /// variables or redirecting).
    #[must_use]
    pub fn is_command(&self) -> bool {
        !self.program.is_empty()
    }
}

/// A command line the parser could not analyse with confidence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub String);

fn err<T>(message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(message.into()))
}

/// Parse a command line into the flat list of every simple command it
/// contains, including those nested in substitutions and subshells.
///
/// # Errors
///
/// Returns a [`ParseError`] when the input uses syntax the parser cannot
/// analyse safely.
pub fn parse_command(input: &str) -> Result<Vec<CommandSegment>, ParseError> {
    parse_at_depth(input, 0)
}

/// [`parse_command`] with an explicit nesting depth.
pub(crate) fn parse_at_depth(input: &str, depth: usize) -> Result<Vec<CommandSegment>, ParseError> {
    if depth > MAX_NESTING_DEPTH {
        return err("the command is nested too deeply");
    }
    let lexer = Lexer::new(input, depth);
    let (tokens, nested) = lexer.run()?;
    let mut segments = build_segments(tokens, depth)?;
    for group in nested {
        merge(&mut segments, group);
    }
    Ok(segments)
}

/// Append `more` to `out`, renumbering its pipelines so they stay distinct.
fn merge(out: &mut Vec<CommandSegment>, more: Vec<CommandSegment>) {
    let base = out.iter().map(|s| s.pipeline + 1).max().unwrap_or(0);
    out.extend(more.into_iter().map(|mut s| {
        s.pipeline += base;
        s
    }));
}

/// Normalise a program word into a comparable program name.
#[must_use]
pub fn normalize_program(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let base = if base.is_empty() { word } else { base };
    let lower = base.to_lowercase();
    lower
        .strip_suffix(".exe")
        .map_or_else(|| lower.clone(), str::to_string)
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Word {
    /// Text after quote removal.
    text: String,
    /// Text as written.
    raw: String,
    /// Whether any part of the word was quoted or escaped.
    quoted: bool,
}

#[derive(Debug)]
enum Token {
    Word(Word),
    Separator { pipe: bool },
    Redirect(String),
}

#[derive(Debug)]
struct PendingHeredoc {
    delimiter: String,
    strip_tabs: bool,
    quoted: bool,
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
    tokens: Vec<Token>,
    nested: Vec<Vec<CommandSegment>>,
    current: Option<Word>,
    heredocs: Vec<PendingHeredoc>,
    /// Set right after `<<` / `<<-`: the next word is a here-doc delimiter.
    expect_delimiter: Option<bool>,
}

impl Lexer {
    fn new(input: &str, depth: usize) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
            depth,
            tokens: Vec::new(),
            nested: Vec::new(),
            current: None,
            heredocs: Vec::new(),
            expect_delimiter: None,
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn word(&mut self) -> &mut Word {
        self.current.get_or_insert_with(Word::default)
    }

    fn push_literal(&mut self, text: &str) {
        let w = self.word();
        w.text.push_str(text);
        w.raw.push_str(text);
    }

    fn finish_word(&mut self) {
        if let Some(w) = self.current.take() {
            if let Some(strip_tabs) = self.expect_delimiter.take() {
                self.heredocs.push(PendingHeredoc {
                    delimiter: w.text.clone(),
                    strip_tabs,
                    quoted: w.quoted,
                });
            }
            self.tokens.push(Token::Word(w));
        }
    }

    fn push_separator(&mut self, pipe: bool) {
        self.finish_word();
        self.tokens.push(Token::Separator { pipe });
    }

    fn slice(&self, from: usize, to: usize) -> String {
        self.chars[from..to].iter().collect()
    }

    fn parse_nested(&mut self, inner: &str) -> Result<(), ParseError> {
        let segments = parse_at_depth(inner, self.depth + 1)?;
        self.nested.push(segments);
        Ok(())
    }

    fn run(mut self) -> Result<(Vec<Token>, Vec<Vec<CommandSegment>>), ParseError> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\r' => {
                    self.finish_word();
                    self.pos += 1;
                }
                '\n' => {
                    self.push_separator(false);
                    self.pos += 1;
                    self.read_heredoc_bodies()?;
                }
                '\\' => match self.peek(1) {
                    Some('\n') => self.pos += 2,
                    Some(n) => {
                        let w = self.word();
                        w.text.push(n);
                        w.raw.push('\\');
                        w.raw.push(n);
                        w.quoted = true;
                        self.pos += 2;
                    }
                    None => return err("the command ends with a dangling backslash"),
                },
                '\'' => self.single_quoted()?,
                '"' => self.double_quoted()?,
                '`' => {
                    let inner = self.backtick()?;
                    self.parse_nested(&inner)?;
                    self.push_literal(&format!("`{inner}`"));
                }
                '$' => self.dollar()?,
                '#' if self.current.is_none() => {
                    while let Some(n) = self.peek(0) {
                        if n == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                ';' | '&' | '|' => self.operator(),
                '<' | '>' => self.redirect()?,
                '(' => {
                    if self.current.is_some() {
                        return err("unexpected `(` inside a word");
                    }
                    let close = find_matching_paren(&self.chars, self.pos)?;
                    let inner = self.slice(self.pos + 1, close);
                    self.push_separator(false);
                    self.parse_nested(&inner)?;
                    self.pos = close + 1;
                    self.push_separator(false);
                }
                ')' => return err("unbalanced `)`"),
                other => {
                    let w = self.word();
                    w.text.push(other);
                    w.raw.push(other);
                    self.pos += 1;
                }
            }
        }
        if self.expect_delimiter.is_some() && self.current.is_none() {
            return err("a here-document is missing its delimiter");
        }
        self.finish_word();
        Ok((self.tokens, self.nested))
    }

    fn single_quoted(&mut self) -> Result<(), ParseError> {
        let start = self.pos + 1;
        let Some(len) = self.chars[start..].iter().position(|&c| c == '\'') else {
            return err("unterminated single quote");
        };
        let content = self.slice(start, start + len);
        let w = self.word();
        w.text.push_str(&content);
        w.raw.push('\'');
        w.raw.push_str(&content);
        w.raw.push('\'');
        w.quoted = true;
        self.pos = start + len + 1;
        Ok(())
    }

    fn double_quoted(&mut self) -> Result<(), ParseError> {
        {
            let w = self.word();
            w.quoted = true;
            w.raw.push('"');
        }
        self.pos += 1;
        loop {
            let Some(c) = self.peek(0) else {
                return err("unterminated double quote");
            };
            match c {
                '"' => {
                    self.word().raw.push('"');
                    self.pos += 1;
                    return Ok(());
                }
                '\\' => match self.peek(1) {
                    Some(n @ ('"' | '\\' | '$' | '`')) => {
                        let w = self.word();
                        w.text.push(n);
                        w.raw.push('\\');
                        w.raw.push(n);
                        self.pos += 2;
                    }
                    Some('\n') => self.pos += 2,
                    _ => {
                        self.push_literal("\\");
                        self.pos += 1;
                    }
                },
                '`' => {
                    let inner = self.backtick()?;
                    self.parse_nested(&inner)?;
                    self.push_literal(&format!("`{inner}`"));
                }
                '$' if self.peek(1) == Some('(') => self.command_substitution()?,
                other => {
                    let w = self.word();
                    w.text.push(other);
                    w.raw.push(other);
                    self.pos += 1;
                }
            }
        }
    }

    /// Read a backtick substitution starting at `self.pos`; returns its body.
    fn backtick(&mut self) -> Result<String, ParseError> {
        let mut i = self.pos + 1;
        let mut inner = String::new();
        while let Some(&c) = self.chars.get(i) {
            match c {
                '\\' => {
                    match self.chars.get(i + 1) {
                        Some(&n @ ('`' | '\\' | '$')) => inner.push(n),
                        Some(&n) => {
                            inner.push('\\');
                            inner.push(n);
                        }
                        None => return err("unterminated backtick substitution"),
                    }
                    i += 2;
                }
                '`' => {
                    self.pos = i + 1;
                    return Ok(inner);
                }
                other => {
                    inner.push(other);
                    i += 1;
                }
            }
        }
        err("unterminated backtick substitution")
    }

    fn dollar(&mut self) -> Result<(), ParseError> {
        match self.peek(1) {
            Some('(') => self.command_substitution(),
            Some('{') => {
                let start = self.pos;
                let Some(len) = self.chars[start..].iter().position(|&c| c == '}') else {
                    return err("unterminated `${`");
                };
                let text = self.slice(start, start + len + 1);
                self.push_literal(&text);
                self.pos = start + len + 1;
                Ok(())
            }
            _ => {
                self.push_literal("$");
                self.pos += 1;
                Ok(())
            }
        }
    }

    /// `$( … )` or `$(( … ))` at `self.pos`.
    fn command_substitution(&mut self) -> Result<(), ParseError> {
        let open = self.pos + 1;
        let close = find_matching_paren(&self.chars, open)?;
        if self.chars.get(open + 1) == Some(&'(') {
            // Arithmetic expansion: no command to extract.
            let text = self.slice(self.pos, close + 1);
            self.push_literal(&text);
        } else {
            let inner = self.slice(open + 1, close);
            self.parse_nested(&inner)?;
            self.push_literal(&format!("$({inner})"));
        }
        self.pos = close + 1;
        Ok(())
    }

    fn operator(&mut self) {
        let c = self.peek(0);
        let n = self.peek(1);
        match (c, n) {
            (Some('&'), Some('>')) => {
                self.finish_word();
                self.pos += 2;
                let op = if self.peek(0) == Some('>') {
                    self.pos += 1;
                    "&>>"
                } else {
                    "&>"
                };
                self.tokens.push(Token::Redirect(op.to_string()));
            }
            (Some('|'), Some('&')) => {
                self.pos += 2;
                self.push_separator(true);
            }
            (Some('&'), Some('&')) | (Some('|'), Some('|')) | (Some(';'), Some(';' | '&')) => {
                self.pos += 2;
                self.push_separator(false);
            }
            (Some('|'), _) => {
                self.pos += 1;
                self.push_separator(true);
            }
            _ => {
                self.pos += 1;
                self.push_separator(false);
            }
        }
    }

    fn redirect(&mut self) -> Result<(), ParseError> {
        let Some(c) = self.peek(0) else {
            return Ok(());
        };
        if self.peek(1) == Some('(') {
            // Process substitution.
            if self.current.is_some() {
                return err("unexpected process substitution inside a word");
            }
            let close = find_matching_paren(&self.chars, self.pos + 1)?;
            let inner = self.slice(self.pos + 2, close);
            self.parse_nested(&inner)?;
            self.push_literal(&format!("{c}(…)"));
            self.finish_word();
            self.pos = close + 1;
            return Ok(());
        }
        // A word made only of digits right before the operator is a file
        // descriptor (`2>`), not an argument.
        if let Some(w) = &self.current
            && !w.quoted
            && !w.text.is_empty()
            && w.text.chars().all(|d| d.is_ascii_digit())
        {
            self.current = None;
        }
        self.finish_word();
        self.pos += 1;
        let mut op = String::from(c);
        let mut heredoc = None;
        if c == '<' {
            match self.peek(0) {
                Some('<') => {
                    self.pos += 1;
                    op.push('<');
                    if self.peek(0) == Some('<') {
                        self.pos += 1; // here-string
                        op.push('<');
                    } else if self.peek(0) == Some('-') {
                        self.pos += 1;
                        op.push('-');
                        heredoc = Some(true);
                    } else {
                        heredoc = Some(false);
                    }
                }
                Some(n @ ('&' | '>')) => {
                    self.pos += 1;
                    op.push(n);
                }
                _ => {}
            }
        } else if let Some(n @ ('>' | '&' | '|')) = self.peek(0) {
            self.pos += 1;
            op.push(n);
        }
        self.tokens.push(Token::Redirect(op));
        self.expect_delimiter = heredoc;
        Ok(())
    }

    fn read_heredoc_bodies(&mut self) -> Result<(), ParseError> {
        if self.expect_delimiter.is_some() {
            return err("a here-document is missing its delimiter");
        }
        let pending = std::mem::take(&mut self.heredocs);
        for doc in pending {
            loop {
                if self.pos >= self.chars.len() {
                    break;
                }
                let end = self.chars[self.pos..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(self.chars.len(), |p| self.pos + p);
                let line = self.slice(self.pos, end);
                self.pos = (end + 1).min(self.chars.len());
                let compared = if doc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if compared.trim_end_matches('\r') == doc.delimiter {
                    break;
                }
                if !doc.quoted && (line.contains("$(") || line.contains('`')) {
                    return err(
                        "command substitution inside an unquoted here-document cannot be \
                         verified; quote the delimiter (<<'EOF') or avoid substitutions",
                    );
                }
            }
        }
        Ok(())
    }
}

/// Index of the `)` matching the `(` at `open`, skipping quoted text.
fn find_matching_paren(chars: &[char], open: usize) -> Result<usize, ParseError> {
    let mut depth = 0usize;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '\'' => match chars[i + 1..].iter().position(|&c| c == '\'') {
                Some(p) => i += p + 1,
                None => return err("unterminated single quote"),
            },
            q @ ('"' | '`') => {
                let mut j = i + 1;
                loop {
                    match chars.get(j) {
                        None => return err("unterminated quote"),
                        Some('\\') => j += 2,
                        Some(&c) if c == q => break,
                        Some(_) => j += 1,
                    }
                }
                i = j;
            }
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    err("unbalanced parenthesis")
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

fn build_segments(tokens: Vec<Token>, depth: usize) -> Result<Vec<CommandSegment>, ParseError> {
    let mut out = Vec::new();
    let mut extras = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut redirections = Vec::new();
    let mut pending: Option<String> = None;
    let mut pipeline = 0usize;
    let mut piped = false;
    let mut flush = |words: &mut Vec<Word>,
                     redirections: &mut Vec<Redirection>,
                     pipeline: usize,
                     piped: bool|
     -> Result<(), ParseError> {
        let (main, extra) =
            simple_command(&std::mem::take(words), std::mem::take(redirections), depth)?;
        if let Some(mut seg) = main {
            seg.pipeline = pipeline;
            seg.piped_input = piped;
            out.push(seg);
        }
        extras.push(extra);
        Ok(())
    };
    for token in tokens {
        match token {
            Token::Redirect(op) => {
                if pending.is_some() {
                    return err("a redirection is missing its target");
                }
                pending = Some(op);
            }
            Token::Word(w) => match pending.take() {
                Some(operator) => redirections.push(Redirection {
                    operator,
                    target: w.text,
                }),
                None => words.push(w),
            },
            Token::Separator { pipe } => {
                if pending.is_some() {
                    return err("a redirection is missing its target");
                }
                flush(&mut words, &mut redirections, pipeline, piped)?;
                piped = pipe;
                if !pipe {
                    pipeline += 1;
                }
            }
        }
    }
    if pending.is_some() {
        return err("a redirection is missing its target");
    }
    flush(&mut words, &mut redirections, pipeline, piped)?;
    for extra in extras {
        merge(&mut out, extra);
    }
    Ok(out)
}

fn is_assignment(word: &str) -> bool {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    for c in chars {
        if c == '=' {
            return true;
        }
        if c == '+' {
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    false
}

/// Split `NAME=value` (or `NAME+=value`) into its name and value.
fn split_assignment(word: &str) -> (String, String) {
    let (name, value) = word.split_once('=').unwrap_or((word, ""));
    (name.trim_end_matches('+').to_string(), value.to_string())
}

/// Options of `xargs` that take a separate value.
const XARGS_VALUE_OPTIONS: &[&str] = &[
    "-I",
    "-n",
    "-L",
    "-P",
    "-s",
    "-d",
    "-E",
    "-a",
    "--max-args",
    "--max-procs",
    "--max-chars",
    "--arg-file",
    "--delimiter",
];

/// State of the wrapper-stripping loop in [`simple_command`].
struct Stripper<'a> {
    words: &'a [Word],
    i: usize,
    depth: usize,
    extra: Vec<CommandSegment>,
}

impl Stripper<'_> {
    fn len(&self) -> usize {
        self.words.len()
    }

    fn text(&self, i: usize) -> &str {
        self.words.get(i).map_or("", |w| w.text.as_str())
    }

    fn current(&self) -> &str {
        self.text(self.i)
    }

    /// Whether any word in `from..self.i` is one of `names`.
    fn saw(&self, from: usize, names: &[&str]) -> bool {
        self.words[from..self.i.min(self.len())]
            .iter()
            .any(|w| names.contains(&w.text.as_str()))
    }

    /// Skip options: `value` options consume the next word, other words
    /// starting with `-` are flags; stops at `--` (consumed) or a positional.
    fn skip_options(&mut self, value: &[&str]) {
        while self.i < self.len() {
            let a = self.current();
            if a == "--" {
                self.i += 1;
                return;
            }
            if value.contains(&a) {
                self.i += 2;
            } else if a.starts_with('-') && a.len() > 1 {
                self.i += 1;
            } else {
                return;
            }
        }
    }

    /// Parse a shell command string passed to a wrapper.
    fn nested(&mut self, command: &str) -> Result<(), ParseError> {
        let segments = parse_at_depth(command, self.depth + 1)?;
        merge(&mut self.extra, segments);
        Ok(())
    }

    /// Values given to any of `names` among the words `from..self.i`, in the
    /// separate (`-o file`), attached (`-ofile`) and `--name=value` forms.
    fn option_values(&self, from: usize, names: &[&str]) -> Vec<String> {
        let mut values = Vec::new();
        let mut j = from;
        while j < self.i.min(self.len()) {
            let w = self.text(j);
            if names.contains(&w) {
                values.push(self.text(j + 1).to_string());
                j += 2;
                continue;
            }
            for name in names {
                let attached = if name.starts_with("--") {
                    w.strip_prefix(name).and_then(|rest| rest.strip_prefix('='))
                } else {
                    w.strip_prefix(name).filter(|rest| !rest.is_empty())
                };
                if let Some(value) = attached {
                    values.push(value.to_string());
                }
            }
            j += 1;
        }
        values
    }

    /// Record a command implied by a wrapper option (`env -C dir` → `cd dir`).
    fn synthetic_command(&mut self, argv: Vec<String>) {
        let segment = CommandSegment {
            program: normalize_program(&argv[0]),
            argv,
            ..CommandSegment::default()
        };
        merge(&mut self.extra, vec![segment]);
    }

    /// Record a file a wrapper writes (`time -o file`, `script file`).
    fn synthetic_write(&mut self, target: &str) {
        let segment = CommandSegment {
            redirections: vec![Redirection {
                operator: ">".into(),
                target: target.to_string(),
            }],
            ..CommandSegment::default()
        };
        merge(&mut self.extra, vec![segment]);
    }

    /// Analyse `words[from..]` as a separate simple command.
    fn nested_words(&mut self, from: usize) -> Result<(), ParseError> {
        let words: Vec<Word> = self.words[from..]
            .iter()
            .map(|w| Word {
                text: w.text.clone(),
                raw: w.raw.clone(),
                quoted: w.quoted,
            })
            .collect();
        let (main, extra) = simple_command(&words, Vec::new(), self.depth)?;
        merge(&mut self.extra, main.into_iter().collect());
        merge(&mut self.extra, extra);
        Ok(())
    }
}

/// What one wrapper did to the stripping loop.
enum Step {
    /// The wrapper was stripped; continue with the word at the new position.
    Stripped,
    /// Not a wrapper, or a wrapper that is itself the program (nothing to
    /// wrap); the program starts at the unchanged position.
    Program,
}

/// Strip one wrapper program starting at `s.i`.
#[allow(clippy::too_many_lines)]
fn strip_wrapper(s: &mut Stripper<'_>, program: &str) -> Result<Step, ParseError> {
    let start = s.i;
    s.i += 1;
    match program {
        "env" => {
            while s.i < s.len() {
                let a = s.current().to_string();
                if a == "--" {
                    s.i += 1;
                    break;
                } else if a.starts_with("-S") || a.starts_with("--split-string") {
                    return err("`env -S` cannot be verified; run the command directly");
                } else if a.starts_with("-P") {
                    return err(
                        "`env -P` changes where programs are looked up; run the command \
                                directly",
                    );
                } else if a == "-C" || a == "--chdir" {
                    let dir = s.text(s.i + 1).to_string();
                    s.synthetic_command(vec!["cd".into(), dir]);
                    s.i += 2;
                } else if let Some(dir) =
                    a.strip_prefix("--chdir=").or_else(|| a.strip_prefix("-C"))
                {
                    s.synthetic_command(vec!["cd".into(), dir.to_string()]);
                    s.i += 1;
                } else if a == "-u" || a == "--unset" {
                    s.i += 2;
                } else if a.starts_with('-') {
                    s.i += 1;
                } else {
                    break;
                }
            }
        }
        "time" => {
            let from = s.i;
            s.skip_options(&["-o", "-f", "--output", "--format"]);
            for file in s.option_values(from, &["-o", "--output"]) {
                s.synthetic_write(&file);
            }
        }
        "nohup" | "builtin" | "setsid" | "chronic" | "unbuffer" => s.skip_options(&[]),
        "nice" => s.skip_options(&["-n", "--adjustment"]),
        "exec" => s.skip_options(&["-a"]),
        "stdbuf" => s.skip_options(&["-i", "-o", "-e", "--input", "--output", "--error"]),
        "caffeinate" => s.skip_options(&["-t", "-w"]),
        "strace" | "ltrace" => {
            let from = s.i;
            s.skip_options(&[
                "-o",
                "-e",
                "-p",
                "-s",
                "-u",
                "-E",
                "-P",
                "-a",
                "-b",
                "-I",
                "-X",
                "-O",
                "-S",
                "-U",
                "-A",
                "-D",
                "-F",
                "-l",
                "-n",
                "-x",
                "--output",
                "--attach",
                "--user",
                "--env",
                "--string-limit",
                "--columns",
                "--library",
            ]);
            for file in s.option_values(from, &["-o", "--output"]) {
                s.synthetic_write(&file);
            }
        }
        "command" => {
            if matches!(s.current(), "-v" | "-V") {
                s.i = start;
                return Ok(Step::Program);
            }
            s.skip_options(&[]);
        }
        "timeout" => {
            s.skip_options(&["-s", "--signal", "-k", "--kill-after"]);
            s.i += 1; // duration
        }
        "ionice" => {
            let from = s.i;
            s.skip_options(&[
                "-c",
                "--class",
                "-n",
                "--classdata",
                "-p",
                "--pid",
                "-P",
                "--pgid",
                "-u",
                "--uid",
            ]);
            if s.saw(from, &["-p", "--pid", "-P", "--pgid", "-u", "--uid"]) {
                s.i = start;
                return Ok(Step::Program);
            }
        }
        "taskset" => {
            let from = s.i;
            s.skip_options(&[]);
            let targets_process = s.words[from..s.i.min(s.len())].iter().any(|w| {
                w.text == "--pid"
                    || (w.text.starts_with('-')
                        && !w.text.starts_with("--")
                        && w.text.contains('p'))
            });
            if targets_process {
                s.i = start;
                return Ok(Step::Program);
            }
            s.i += 1; // CPU mask or list
        }
        "flock" => {
            let mut lock_file_seen = false;
            while s.i < s.len() {
                let a = s.current().to_string();
                if a == "-c" || a == "--command" {
                    let command = s.text(s.i + 1).to_string();
                    s.nested(&command)?;
                    s.i += 2;
                } else if let Some(command) = a.strip_prefix("--command=") {
                    s.nested(command)?;
                    s.i += 1;
                } else if matches!(
                    a.as_str(),
                    "-w" | "--timeout" | "-E" | "--conflict-exit-code"
                ) {
                    s.i += 2;
                } else if a.starts_with('-') && a.len() > 1 {
                    s.i += 1;
                } else if !lock_file_seen {
                    lock_file_seen = true;
                    s.i += 1;
                } else {
                    break;
                }
            }
        }
        "watch" => {
            let from = s.i;
            s.skip_options(&["-n", "--interval", "-q", "--equexit"]);
            if !s.saw(from, &["-x", "--exec"]) && s.i < s.len() {
                // `watch` joins its arguments and runs them through `sh -c`.
                let command = s.words[s.i..]
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                s.nested(&command)?;
                s.i = start;
                return Ok(Step::Program);
            }
        }
        "script" => {
            let mut positionals = Vec::new();
            let mut takes_time = false;
            while s.i < s.len() {
                let a = s.current().to_string();
                if a == "-c" || a == "--command" {
                    let command = s.text(s.i + 1).to_string();
                    s.nested(&command)?;
                    s.i += 2;
                } else if let Some(command) = a.strip_prefix("--command=") {
                    s.nested(command)?;
                    s.i += 1;
                } else if matches!(
                    a.as_str(),
                    "-E" | "--echo"
                        | "-o"
                        | "--output-limit"
                        | "-T"
                        | "--log-timing"
                        | "-I"
                        | "--log-in"
                        | "-O"
                        | "--log-out"
                        | "-B"
                        | "--log-io"
                        | "-m"
                        | "--logging-format"
                ) {
                    s.i += 2;
                } else if a.starts_with('-') && a.len() > 1 {
                    takes_time |= a == "-t";
                    s.i += 1;
                } else {
                    positionals.push(s.i);
                    s.i += 1;
                }
            }
            // The first positional is the transcript file (truncated). BSD
            // form: `script [file [command …]]`. With BSD `-t time` the first
            // positional may be the time, so both readings are checked.
            for &at in positionals.iter().take(if takes_time { 2 } else { 1 }) {
                let file = s.text(at).to_string();
                s.synthetic_write(&file);
            }
            if let Some(&from) = positionals.get(1) {
                s.nested_words(from)?;
            }
            if takes_time && let Some(&from) = positionals.get(2) {
                s.nested_words(from)?;
            }
            s.i = start;
            return Ok(Step::Program);
        }
        "busybox" | "toybox" => {
            if s.i >= s.len() || s.current().starts_with('-') {
                s.i = start;
                return Ok(Step::Program);
            }
        }
        "xargs" => {
            let mut replace: Vec<String> = Vec::new();
            while s.i < s.len() {
                let a = s.current().to_string();
                if a.starts_with("--process-slot-var") {
                    return err("`xargs --process-slot-var` is not allowed");
                } else if a == "-I" {
                    replace.push(s.text(s.i + 1).to_string());
                    s.i += 2;
                } else if a == "-i" || a == "--replace" {
                    replace.push("{}".into());
                    s.i += 1;
                } else if let Some(r) = a.strip_prefix("--replace=") {
                    replace.push(r.to_string());
                    s.i += 1;
                } else if let Some(r) = a.strip_prefix("-I").or_else(|| a.strip_prefix("-i")) {
                    replace.push(r.to_string());
                    s.i += 1;
                } else if XARGS_VALUE_OPTIONS.contains(&a.as_str()) {
                    s.i += 2;
                } else if a == "--" {
                    s.i += 1;
                    break;
                } else if a.starts_with('-') && a.len() > 1 {
                    s.i += 1;
                } else {
                    break;
                }
            }
            if s.i >= s.len() {
                return err(
                    "`xargs` must name the program to run explicitly (for example `xargs rm`)",
                );
            }
            let program = s.current();
            if replace
                .iter()
                .any(|r| !r.is_empty() && program.contains(r.as_str()))
            {
                return err("the program run by `xargs` would come from its input");
            }
        }
        _ => {
            s.i = start;
            return Ok(Step::Program);
        }
    }
    Ok(Step::Stripped)
}

/// Turn the words of one simple command into a segment, recording
/// assignments and stripping shell keywords and wrapper programs. Returns the
/// main segment and any segments parsed from command strings given to
/// wrappers.
fn simple_command(
    words: &[Word],
    redirections: Vec<Redirection>,
    depth: usize,
) -> Result<(Option<CommandSegment>, Vec<CommandSegment>), ParseError> {
    let mut s = Stripper {
        words,
        i: 0,
        depth,
        extra: Vec::new(),
    };
    let mut assignments = Vec::new();
    let mut wrappers: Vec<String> = Vec::new();
    let mut last_wrapper: Option<usize> = None;
    let mut after_env = false;
    loop {
        while s.i < s.len()
            && (is_assignment(&words[s.i].raw) || (after_env && is_assignment(&words[s.i].text)))
        {
            assignments.push(split_assignment(&words[s.i].text));
            s.i += 1;
        }
        after_env = false;
        if s.i >= s.len() {
            break;
        }
        if !words[s.i].quoted {
            match s.current() {
                "{" | "}" | "!" | "if" | "then" | "else" | "elif" | "fi" | "do" | "done"
                | "while" | "until" | "esac" => {
                    s.i += 1;
                    continue;
                }
                "for" | "select" => {
                    // `for NAME in …`: only the loop variable matters.
                    let name = s.text(s.i + 1).to_string();
                    assignments.push((name, String::new()));
                    s.i = s.len();
                    break;
                }
                kw @ ("case" | "function") => {
                    return err(format!(
                        "`{kw}` constructs are not supported by the command checker; \
                         use simpler commands"
                    ));
                }
                _ => {}
            }
        }
        let start = s.i;
        let program = normalize_program(s.current());
        match strip_wrapper(&mut s, &program)? {
            Step::Stripped => {
                after_env = program == "env";
                wrappers.push(program);
                last_wrapper = Some(start);
            }
            Step::Program => break,
        }
    }
    let extra = std::mem::take(&mut s.extra);
    let from = if s.i < words.len() {
        Some(s.i)
    } else if let Some(w) = last_wrapper {
        // The last wrapper had nothing to wrap: it is the program itself.
        wrappers.pop();
        Some(w)
    } else {
        None
    };
    let segment = match from {
        Some(from) => {
            let argv: Vec<String> = words[from..].iter().map(|w| w.text.clone()).collect();
            Some(CommandSegment {
                program: normalize_program(&argv[0]),
                argv,
                assignments,
                redirections,
                wrappers,
                ..CommandSegment::default()
            })
        }
        None if !assignments.is_empty() || !redirections.is_empty() => Some(CommandSegment {
            assignments,
            redirections,
            ..CommandSegment::default()
        }),
        None => None,
    };
    Ok((segment, extra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn segments(cmd: &str) -> Vec<CommandSegment> {
        parse_command(cmd).unwrap_or_else(|e| panic!("`{cmd}` failed: {e}"))
    }

    fn programs(cmd: &str) -> Vec<String> {
        segments(cmd)
            .into_iter()
            .filter(CommandSegment::is_command)
            .map(|s| s.program)
            .collect()
    }

    fn argv(cmd: &str) -> Vec<Vec<String>> {
        segments(cmd)
            .into_iter()
            .filter(CommandSegment::is_command)
            .map(|s| s.argv)
            .collect()
    }

    #[test]
    fn splits_on_all_operators() {
        assert_eq!(
            programs("a | b || c && d ; e & f\ng |& h"),
            ["a", "b", "c", "d", "e", "f", "g", "h"]
        );
    }

    #[test]
    fn pipelines_are_tracked() {
        let segs = segments("a | b && c | d");
        let ids: Vec<(usize, bool)> = segs.iter().map(|s| (s.pipeline, s.piped_input)).collect();
        assert_eq!(ids[0].0, ids[1].0);
        assert_eq!(ids[2].0, ids[3].0);
        assert_ne!(ids[0].0, ids[2].0);
        assert_eq!(
            ids.iter().map(|x| x.1).collect::<Vec<_>>(),
            [false, true, false, true]
        );
        // Nested pipelines get distinct identifiers.
        let segs = segments("a | b $(c | d)");
        let outer = segs[0].pipeline;
        assert_eq!(segs.iter().filter(|s| s.pipeline == outer).count(), 2);
    }

    #[test]
    fn empty_input_has_no_segments() {
        assert!(parse_command("").unwrap().is_empty());
        assert!(parse_command("   \n ; ").unwrap().is_empty());
        assert!(parse_command("# only a comment").unwrap().is_empty());
    }

    #[test]
    fn quotes_are_removed() {
        assert_eq!(
            argv(r#"echo 'a b' "c $d" e\ f"#),
            [vec!["echo", "a b", "c $d", "e f"]]
        );
        assert_eq!(programs(r"\rm x"), ["rm"]);
        assert_eq!(programs("'r''m' x"), ["rm"]);
    }

    #[test]
    fn operators_inside_quotes_do_not_split() {
        assert_eq!(programs("echo 'a && b; c | d'"), ["echo"]);
        assert_eq!(programs(r#"grep "x|y" f"#), ["grep"]);
    }

    #[test]
    fn command_substitutions_are_extracted() {
        assert_eq!(programs("echo $(rm -rf /)"), ["echo", "rm"]);
        assert_eq!(programs("echo \"$(whoami)\""), ["echo", "whoami"]);
        assert_eq!(programs("echo `uname -a`"), ["echo", "uname"]);
        assert_eq!(programs("echo $(a $(b))"), ["echo", "a", "b"]);
        assert_eq!(programs("x=$(sudo id)"), ["sudo"]);
    }

    #[test]
    fn arithmetic_is_not_a_command() {
        assert_eq!(programs("echo $((1 + 2))"), ["echo"]);
    }

    #[test]
    fn subshells_and_process_substitution() {
        assert_eq!(programs("(cd x && make)"), ["cd", "make"]);
        assert_eq!(programs("diff <(ls a) <(ls b)"), ["diff", "ls", "ls"]);
        assert_eq!(programs("{ echo a; echo b; }"), ["echo", "echo"]);
    }

    #[test]
    fn redirections_are_recorded() {
        let seg = &segments("cargo test 2>&1 > out.txt < in.txt")[0];
        assert_eq!(seg.argv, ["cargo", "test"]);
        let ops: Vec<(&str, &str, bool)> = seg
            .redirections
            .iter()
            .map(|r| (r.operator.as_str(), r.target.as_str(), r.writes_file()))
            .collect();
        assert_eq!(
            ops,
            [
                (">&", "1", false),
                (">", "out.txt", true),
                ("<", "in.txt", false)
            ]
        );
        let seg = &segments("ls &>> log")[0];
        assert_eq!(seg.redirections[0].operator, "&>>");
        // A redirection without a command still produces a segment.
        let seg = &segments("> ~/x")[0];
        assert!(!seg.is_command());
        assert_eq!(seg.redirections[0].target, "~/x");
        let seg = &segments("{ ls; } > f")[1];
        assert_eq!(seg.redirections[0].target, "f");
    }

    #[test]
    fn heredoc_bodies_are_skipped() {
        let cmd = "cat <<'EOF' > f.txt\nrm -rf / $(x)\nEOF\necho done";
        assert_eq!(programs(cmd), ["cat", "echo"]);
        let cmd = "cat <<-END\n\tplain text\n\tEND\nls";
        assert_eq!(programs(cmd), ["cat", "ls"]);
    }

    #[test]
    fn unquoted_heredoc_with_substitution_is_rejected() {
        assert!(parse_command("cat <<EOF\n$(rm -rf /)\nEOF").is_err());
    }

    #[test]
    fn assignments_are_recorded() {
        let seg = &segments("FOO=bar BAZ='q x' cargo build")[0];
        assert_eq!(seg.argv, ["cargo", "build"]);
        assert_eq!(
            seg.assignments,
            [("FOO".into(), "bar".into()), ("BAZ".into(), "q x".into())]
        );
        let seg = &segments("env -i 'PATH=/tmp' A+=1 ls")[0];
        assert_eq!(seg.program, "ls");
        assert_eq!(seg.wrappers, ["env"]);
        assert_eq!(
            seg.assignments,
            [("PATH".into(), "/tmp".into()), ("A".into(), "1".into())]
        );
        let seg = &segments("PATH=/tmp")[0];
        assert!(!seg.is_command());
        assert_eq!(seg.assignments[0].0, "PATH");
        let seg = &segments("for PATH in a b; do ls; done")[0];
        assert_eq!(seg.assignments[0].0, "PATH");
    }

    #[test]
    fn classic_wrappers_are_stripped() {
        assert_eq!(programs("env -i FOO=1 sudo ls"), ["sudo"]);
        assert_eq!(programs("env -u HOME rm x"), ["rm"]);
        assert_eq!(programs("time -p nice -n 10 nohup make"), ["make"]);
        assert_eq!(programs("exec rm x"), ["rm"]);
        assert_eq!(programs("timeout -s KILL 10 sudo x"), ["sudo"]);
        assert_eq!(programs("xargs -n 1 rm"), ["rm"]);
        assert_eq!(programs("command -v git"), ["command"]);
        assert_eq!(programs("command git status"), ["git"]);
        assert_eq!(programs("nohup"), ["nohup"]);
        let seg = &segments("nice -n 5 nohup make")[0];
        assert_eq!(seg.wrappers, ["nice", "nohup"]);
    }

    #[test]
    fn new_wrappers_are_stripped() {
        for (cmd, expected) in [
            ("setsid -f sudo id", vec!["sudo"]),
            ("stdbuf -oL -e 0 sudo id", vec!["sudo"]),
            ("caffeinate -i -t 10 sudo id", vec!["sudo"]),
            ("chronic -e sudo id", vec!["sudo"]),
            ("unbuffer -p sudo id", vec!["sudo"]),
            ("ionice -c 3 sudo id", vec!["sudo"]),
            ("ionice -p 42", vec!["ionice"]),
            ("taskset 0x1 sudo id", vec!["sudo"]),
            ("taskset -c 0-3 sudo id", vec!["sudo"]),
            ("taskset -p 0x1 42", vec!["taskset"]),
            ("flock /tmp/lock sudo id", vec!["sudo"]),
            ("flock -w 5 /tmp/lock -c 'sudo id'", vec!["flock", "sudo"]),
            ("watch -n 1 sudo id", vec!["watch", "sudo"]),
            ("watch -x sudo id", vec!["sudo"]),
            ("strace -f -o log sudo id", vec!["sudo"]),
            ("ltrace -e malloc sudo id", vec!["sudo"]),
            ("script -q -c 'sudo id' /dev/null", vec!["script", "sudo"]),
            ("script -q out.log sudo id", vec!["script", "sudo"]),
            (
                "script -t 0 out.log sudo id",
                vec!["script", "out.log", "sudo"],
            ),
            ("busybox reboot", vec!["reboot"]),
            ("toybox reboot", vec!["reboot"]),
            ("busybox --list", vec!["busybox"]),
        ] {
            assert_eq!(programs(cmd), expected, "{cmd}");
        }
    }

    #[test]
    fn xargs_rules() {
        assert!(parse_command("xargs").is_err());
        assert!(parse_command("ls | xargs -0").is_err());
        assert!(parse_command("xargs -I CMD CMD x").is_err());
        assert!(parse_command("xargs -ICMD CMD x").is_err());
        assert!(parse_command("xargs --process-slot-var=X sh").is_err());
        assert_eq!(programs("xargs -I{} cp {} dst/"), ["cp"]);
        let seg = &segments("ls | xargs -0 rm -f")[1];
        assert_eq!(seg.program, "rm");
        assert_eq!(seg.wrappers, ["xargs"]);
        assert!(seg.piped_input);
    }

    #[test]
    fn program_is_normalised() {
        assert_eq!(programs("/usr/bin/sudo ls"), ["sudo"]);
        assert_eq!(
            programs(r"C:\\Windows\\System32\\SHUTDOWN.EXE /s"),
            ["shutdown"]
        );
        assert_eq!(normalize_program("Git.exe"), "git");
    }

    #[test]
    fn control_flow_keywords_are_skipped() {
        assert_eq!(
            programs("if test -f x; then rm x; else touch x; fi"),
            ["test", "rm", "touch"]
        );
        assert_eq!(programs("for f in a b; do echo $f; done"), ["echo"]);
        assert_eq!(programs("while true; do sleep 1; done"), ["true", "sleep"]);
    }

    #[test]
    fn line_continuation_and_comments() {
        assert_eq!(
            argv("cargo \\\n  build # trailing"),
            [vec!["cargo", "build"]]
        );
        assert_eq!(argv("echo a#b"), [vec!["echo", "a#b"]]);
    }

    #[test]
    fn malformed_input_is_rejected() {
        for bad in [
            "echo 'unterminated",
            "echo \"unterminated",
            "echo $(unclosed",
            "echo `unclosed",
            "echo )",
            "echo a(b",
            "case x in a) ls;; esac",
            "env -S 'rm -rf /'",
            "ls >",
            "cat <<",
            "echo \\",
        ] {
            assert!(parse_command(bad).is_err(), "`{bad}` should be rejected");
        }
    }

    #[test]
    fn nesting_depth_is_limited() {
        let mut cmd = String::from("ls");
        for _ in 0..=MAX_NESTING_DEPTH + 1 {
            cmd = format!("echo $({cmd})");
        }
        assert!(parse_command(&cmd).is_err());
    }

    #[test]
    fn args_accessor() {
        let seg = &parse_command("git status -s").unwrap()[0];
        assert_eq!(seg.args(), ["status", "-s"]);
        assert!(CommandSegment::default().args().is_empty());
    }
}
