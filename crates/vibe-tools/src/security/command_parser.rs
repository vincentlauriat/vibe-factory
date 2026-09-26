//! A conservative POSIX shell command-line parser.
//!
//! The parser does not execute or expand anything. Its only job is to find
//! every program a command line would start, so that the security policy can
//! check each of them. It therefore:
//!
//! - splits on `|`, `||`, `|&`, `&&`, `;`, `&` and newlines;
//! - recursively extracts the commands inside `$( … )`, backticks, subshells
//!   `( … )` and process substitutions `<( … )` / `>( … )`;
//! - drops redirections (`>`, `2>&1`, `<<EOF` …) and their targets, and skips
//!   here-document bodies;
//! - strips leading environment assignments (`FOO=bar cmd`) and wrapper
//!   programs (`env`, `time`, `nice`, `nohup`, `exec`, `command`, `builtin`,
//!   `timeout`, `xargs`);
//! - removes quotes and backslash escapes from every word.
//!
//! Anything it does not understand (unbalanced quotes or parentheses, `case`
//! statements, `env -S`, …) is reported as a [`ParseError`]. Callers must treat
//! an error as a denial.

/// Maximum nesting of command substitutions, subshells and `sh -c` strings.
pub const MAX_NESTING_DEPTH: usize = 16;

/// One simple command found in a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSegment {
    /// Normalised program name: the last path component of the first word,
    /// lowercased, with a trailing `.exe` removed (`/usr/bin/Git.exe` → `git`).
    pub program: String,
    /// The words of the command after quote removal, starting with the
    /// program exactly as written.
    pub argv: Vec<String>,
}

impl CommandSegment {
    /// Arguments after the program word.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.argv[1..]
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
    let mut segments = build_segments(tokens)?;
    segments.extend(nested);
    Ok(segments)
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
    Separator,
    Redirect,
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
    nested: Vec<CommandSegment>,
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

    fn push_separator(&mut self) {
        self.finish_word();
        self.tokens.push(Token::Separator);
    }

    fn slice(&self, from: usize, to: usize) -> String {
        self.chars[from..to].iter().collect()
    }

    fn parse_nested(&mut self, inner: &str) -> Result<(), ParseError> {
        let segments = parse_at_depth(inner, self.depth + 1)?;
        self.nested.extend(segments);
        Ok(())
    }

    fn run(mut self) -> Result<(Vec<Token>, Vec<CommandSegment>), ParseError> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\r' => {
                    self.finish_word();
                    self.pos += 1;
                }
                '\n' => {
                    self.push_separator();
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
                    self.push_separator();
                    self.parse_nested(&inner)?;
                    self.pos = close + 1;
                    self.push_separator();
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
                if self.peek(0) == Some('>') {
                    self.pos += 1;
                }
                self.tokens.push(Token::Redirect);
            }
            (Some('&'), Some('&'))
            | (Some('|'), Some('|' | '&'))
            | (Some(';'), Some(';' | '&')) => {
                self.pos += 2;
                self.push_separator();
            }
            _ => {
                self.pos += 1;
                self.push_separator();
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
        let mut heredoc = None;
        if c == '<' {
            match self.peek(0) {
                Some('<') => {
                    self.pos += 1;
                    if self.peek(0) == Some('<') {
                        self.pos += 1; // here-string
                    } else if self.peek(0) == Some('-') {
                        self.pos += 1;
                        heredoc = Some(true);
                    } else {
                        heredoc = Some(false);
                    }
                }
                Some('&' | '>') => self.pos += 1,
                _ => {}
            }
        } else if matches!(self.peek(0), Some('>' | '&' | '|')) {
            self.pos += 1;
        }
        self.tokens.push(Token::Redirect);
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

fn build_segments(tokens: Vec<Token>) -> Result<Vec<CommandSegment>, ParseError> {
    let mut out = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut expect_target = false;
    for token in tokens {
        match token {
            Token::Redirect => {
                if expect_target {
                    return err("a redirection is missing its target");
                }
                expect_target = true;
            }
            Token::Word(w) => {
                if expect_target {
                    expect_target = false;
                } else {
                    words.push(w);
                }
            }
            Token::Separator => {
                if expect_target {
                    return err("a redirection is missing its target");
                }
                if let Some(seg) = simple_command(&std::mem::take(&mut words))? {
                    out.push(seg);
                }
            }
        }
    }
    if expect_target {
        return err("a redirection is missing its target");
    }
    if let Some(seg) = simple_command(&words)? {
        out.push(seg);
    }
    Ok(out)
}

fn is_assignment(raw: &str) -> bool {
    let mut chars = raw.chars();
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

/// Options of `xargs` that take a separate value.
const XARGS_VALUE_OPTIONS: &[&str] = &["-I", "-n", "-L", "-P", "-s", "-d", "-E", "-a"];

/// Turn the words of one simple command into a segment, stripping
/// assignments, shell keywords and wrapper programs.
fn simple_command(words: &[Word]) -> Result<Option<CommandSegment>, ParseError> {
    let text = |i: usize| words[i].text.as_str();
    let len = words.len();
    let mut i = 0;
    loop {
        while i < len && is_assignment(&words[i].raw) {
            i += 1;
        }
        if i >= len {
            return Ok(None);
        }
        if !words[i].quoted {
            match text(i) {
                "{" | "}" | "!" | "if" | "then" | "else" | "elif" | "fi" | "do" | "done"
                | "while" | "until" | "esac" => {
                    i += 1;
                    continue;
                }
                "for" | "select" => return Ok(None),
                kw @ ("case" | "function") => {
                    return err(format!(
                        "`{kw}` constructs are not supported by the command checker; \
                         use simpler commands"
                    ));
                }
                _ => {}
            }
        }
        match normalize_program(text(i)).as_str() {
            "env" => {
                i += 1;
                while i < len {
                    let a = text(i);
                    if a == "--" {
                        i += 1;
                        break;
                    } else if a == "-S" || a.starts_with("-S") || a.starts_with("--split-string") {
                        return err("`env -S` cannot be verified; run the command directly");
                    } else if matches!(a, "-u" | "--unset" | "-C" | "--chdir") {
                        i += 2;
                    } else if a.starts_with('-') {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            "time" => {
                i += 1;
                while i < len && text(i).starts_with('-') {
                    let done = text(i) == "--";
                    i += 1;
                    if done {
                        break;
                    }
                }
            }
            "nice" => {
                i += 1;
                while i < len {
                    let a = text(i);
                    if a == "-n" || a == "--adjustment" {
                        i += 2;
                    } else if a.starts_with('-') {
                        let done = a == "--";
                        i += 1;
                        if done {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            "nohup" | "builtin" => {
                i += 1;
                if i < len && text(i) == "--" {
                    i += 1;
                }
            }
            "exec" => {
                i += 1;
                while i < len {
                    match text(i) {
                        "-a" => i += 2,
                        "--" => {
                            i += 1;
                            break;
                        }
                        a if a.starts_with('-') => i += 1,
                        _ => break,
                    }
                }
            }
            "command" => {
                if i + 1 < len && matches!(text(i + 1), "-v" | "-V") {
                    break;
                }
                i += 1;
                while i < len && text(i).starts_with('-') {
                    let done = text(i) == "--";
                    i += 1;
                    if done {
                        break;
                    }
                }
            }
            "timeout" => {
                i += 1;
                while i < len {
                    let a = text(i);
                    if matches!(a, "-s" | "--signal" | "-k" | "--kill-after") {
                        i += 2;
                    } else if a == "--" {
                        i += 1;
                        break;
                    } else if a.starts_with('-') {
                        i += 1;
                    } else {
                        break;
                    }
                }
                i += 1; // duration
            }
            "xargs" => {
                i += 1;
                while i < len {
                    let a = text(i);
                    if XARGS_VALUE_OPTIONS.contains(&a) {
                        i += 2;
                    } else if a == "--" {
                        i += 1;
                        break;
                    } else if a.starts_with('-') {
                        i += 1;
                    } else {
                        break;
                    }
                }
                if i >= len {
                    return Ok(Some(CommandSegment {
                        program: "echo".into(),
                        argv: vec!["echo".into()],
                    }));
                }
            }
            _ => break,
        }
    }
    let argv: Vec<String> = words[i..].iter().map(|w| w.text.clone()).collect();
    Ok(Some(CommandSegment {
        program: normalize_program(&argv[0]),
        argv,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn programs(cmd: &str) -> Vec<String> {
        parse_command(cmd)
            .unwrap_or_else(|e| panic!("`{cmd}` failed: {e}"))
            .into_iter()
            .map(|s| s.program)
            .collect()
    }

    fn argv(cmd: &str) -> Vec<Vec<String>> {
        parse_command(cmd)
            .unwrap()
            .into_iter()
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
    fn redirections_are_dropped() {
        assert_eq!(
            argv("cargo test 2>&1 > out.txt < in.txt"),
            [vec!["cargo", "test"]]
        );
        assert_eq!(argv("ls &> log"), [vec!["ls"]]);
        assert_eq!(argv("echo hi >> f"), [vec!["echo", "hi"]]);
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
    fn env_assignments_and_wrappers_are_stripped() {
        assert_eq!(
            argv("FOO=bar BAZ='q x' cargo build"),
            [vec!["cargo", "build"]]
        );
        assert_eq!(programs("env -i FOO=1 sudo ls"), ["sudo"]);
        assert_eq!(programs("env -u HOME rm x"), ["rm"]);
        assert_eq!(programs("time -p nice -n 10 nohup make"), ["make"]);
        assert_eq!(programs("exec rm x"), ["rm"]);
        assert_eq!(programs("timeout -s KILL 10 sudo x"), ["sudo"]);
        assert_eq!(programs("xargs -n 1 rm"), ["rm"]);
        assert_eq!(programs("xargs"), ["echo"]);
        assert_eq!(programs("command -v git"), ["command"]);
        assert_eq!(programs("command git status"), ["git"]);
        assert_eq!(programs("FOO=1"), Vec::<String>::new());
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
    }
}
