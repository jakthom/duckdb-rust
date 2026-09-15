mod directives;
#[allow(dead_code)]
mod fixture_bench;
#[allow(dead_code)]
mod fixtures;
#[allow(dead_code)]
mod oracle;
#[allow(dead_code)]
mod parser;

use directives::{DirectiveAction, DirectiveHeader, DirectiveState, Mode};
use duckdb_rust::{Connection, DataType, Database, Error, QueryResult, Result, Value};
use fixtures::FixtureResolver;
use oracle::{
    ActualCell, ActualColumn, ActualError, ActualResult, ErrorKind, ExpectedStatement,
    ExpectedSubstitutions, ExpectedValues, Oracle, QueryExpectation, Re2Matcher, SortMode,
    SourceRootFileResolver, StatementResult,
};
use parser::{ExecutionOutcome, RecordAccounting, SqlLogicParser, Token, TokenKind};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FileStatus {
    Passed,
    Skipped(String),
    /// At least one source command deliberately bypassed an expectation while
    /// emitting replacement output (currently `mode output_hash` or a
    /// statement-level `debug`/`debug_skip`). The source run succeeded, but it
    /// is not an assertion-backed parity pass.
    GeneratedOutput(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileReport {
    pub status: FileStatus,
    pub declarations: usize,
    pub passed: usize,
    pub skipped: usize,
    pub generated: usize,
    /// Output requested by `mode output_*`, `mode debug`, or statement-level
    /// `debug`/`debug_skip`. Keeping it in the report makes the source modes
    /// observable without writing nondeterministically during test execution.
    pub output: Vec<String>,
}

struct Header {
    location: directives::SourceLocation,
    keyword: String,
    arguments: Vec<String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DirectiveHeader for Header {
    fn location(&self) -> &directives::SourceLocation {
        &self.location
    }

    fn keyword(&self) -> &str {
        &self.keyword
    }

    fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

#[derive(Default)]
struct Substitutions(RefCell<BTreeMap<Vec<u8>, Vec<u8>>>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Substitutions {
    fn insert(&self, name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.0
            .borrow_mut()
            .insert(name.as_ref().to_vec(), value.as_ref().to_vec());
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpectedSubstitutions for Substitutions {
    fn replace(&self, input: &[u8]) -> Vec<u8> {
        let mut result = input.to_vec();
        for (name, value) in self.0.borrow().iter() {
            for marker in [
                [b"${".as_slice(), name, b"}"].concat(),
                [b"{".as_slice(), name, b"}"].concat(),
            ] {
                result = replace_all(&result, &marker, value);
            }
        }
        result
    }
}

struct RustRe2Matcher;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Re2Matcher for RustRe2Matcher {
    fn full_match(&self, pattern: &[u8], value: &[u8]) -> std::result::Result<bool, String> {
        let pattern = std::str::from_utf8(pattern)
            .map_err(|_| "RE2 pattern is not valid UTF-8".to_string())?;
        let pattern = translate_re2_pattern(pattern)?;
        // Anchoring the expression implements RE2 FullMatch. Both engines use
        // a linear-time automaton and reject look-around and backreferences.
        let expression = format!(r"\A(?:{pattern})\z");
        regex::bytes::RegexBuilder::new(&expression)
            .dot_matches_new_line(true)
            // Keep Unicode mode enabled so `.`, classes and their quantifiers
            // consume UTF-8 codepoints like RE2. The adapter above narrows only
            // RE2's ASCII Perl classes and word-boundary assertions.
            .unicode(true)
            .build()
            .map(|regex| regex.is_match(value))
            .map_err(|error| error.to_string())
    }
}

/// Translate the small part of Rust's regex dialect that differs from RE2.
/// RE2's generated `perl_groups.cc` defines Perl classes over ASCII ranges,
/// while Rust makes them Unicode-aware by default. RE2 also treats `[`, `&`
/// and `~` as ordinary class runes instead of nested-class/set operators.
/// Keeping Unicode mode enabled preserves RE2 rune semantics for `.` and
/// quantifiers; only the differing constructs are rewritten here.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn translate_re2_pattern(pattern: &str) -> std::result::Result<String, String> {
    let mut output = String::with_capacity(pattern.len());
    let mut cursor = 0;
    while cursor < pattern.len() {
        let character = next_character(pattern, cursor);
        if character == '\\' {
            output.push(character);
            cursor += character.len_utf8();
            if cursor == pattern.len() {
                break;
            }
            let escaped = next_character(pattern, cursor);
            output.pop();
            cursor += escaped.len_utf8();
            match escaped {
                'd' => output.push_str("[0-9]"),
                'D' => output.push_str("[^0-9]"),
                's' => output.push_str(r"[\x09-\x0A\x0C-\x0D\x20]"),
                'S' => output.push_str(r"[^\x09-\x0A\x0C-\x0D\x20]"),
                'w' => output.push_str("[0-9A-Z_a-z]"),
                'W' => output.push_str("[^0-9A-Z_a-z]"),
                // RE2's Perl word boundaries use the ASCII `\w` definition.
                'b' | 'B' => {
                    output.push_str("(?-u:\\");
                    output.push(escaped);
                    output.push(')');
                }
                _ => {
                    output.push(character);
                    output.push(escaped);
                }
            }
            continue;
        }
        if character == '[' {
            let (class, consumed) = translate_re2_class(&pattern[cursor..])?;
            output.push_str(&class);
            cursor += consumed;
            continue;
        }
        output.push(character);
        cursor += character.len_utf8();
    }
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn translate_re2_class(input: &str) -> std::result::Result<(String, usize), String> {
    debug_assert!(input.starts_with('['));
    let mut output = String::from("[");
    let mut cursor = 1;
    if input[cursor..].starts_with('^') {
        output.push('^');
        cursor += 1;
    }
    let mut first = true;
    loop {
        if cursor == input.len() {
            return Err("unclosed RE2 character class".to_string());
        }
        if input[cursor..].starts_with(']') && !first {
            output.push(']');
            return Ok((output, cursor + 1));
        }
        first = false;

        if let Some(consumed) = re2_posix_class_len(&input[cursor..]) {
            output.push_str(&input[cursor..cursor + consumed]);
            cursor += consumed;
            continue;
        }
        if let Some(consumed) = re2_unicode_class_len(&input[cursor..])? {
            output.push_str(&input[cursor..cursor + consumed]);
            cursor += consumed;
            continue;
        }
        if let Some((class, consumed)) = re2_perl_class(&input[cursor..]) {
            output.push_str(class);
            cursor += consumed;
            continue;
        }

        let (low, consumed) = parse_re2_class_character(&input[cursor..])?;
        cursor += consumed;
        if input[cursor..].starts_with('-')
            && input.len() > cursor + 1
            && !input[cursor + 1..].starts_with(']')
        {
            cursor += 1;
            let (high, consumed) = parse_re2_class_character(&input[cursor..])?;
            cursor += consumed;
            if high < low {
                return Err(format!(
                    "invalid RE2 character class range U+{:04X}-U+{:04X}",
                    low as u32, high as u32
                ));
            }
            push_class_rune(&mut output, low);
            output.push('-');
            push_class_rune(&mut output, high);
        } else {
            push_class_rune(&mut output, low);
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn re2_posix_class_len(input: &str) -> Option<usize> {
    input.strip_prefix("[:")?.find(":]").map(|end| 2 + end + 2)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn re2_unicode_class_len(input: &str) -> std::result::Result<Option<usize>, String> {
    let Some(rest) = input
        .strip_prefix("\\p")
        .or_else(|| input.strip_prefix("\\P"))
    else {
        return Ok(None);
    };
    let Some(first) = rest.chars().next() else {
        return Err("incomplete RE2 Unicode character class".to_string());
    };
    if first != '{' {
        return Ok(Some(2 + first.len_utf8()));
    }
    let Some(end) = rest.find('}') else {
        return Err("unclosed RE2 Unicode character class".to_string());
    };
    Ok(Some(2 + end + 1))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn re2_perl_class(input: &str) -> Option<(&'static str, usize)> {
    let class = match input.as_bytes().get(1).copied()? {
        b'd' => "[0-9]",
        b'D' => "[^0-9]",
        b's' => r"[\x09-\x0A\x0C-\x0D\x20]",
        b'S' => r"[^\x09-\x0A\x0C-\x0D\x20]",
        b'w' => "[0-9A-Z_a-z]",
        b'W' => "[^0-9A-Z_a-z]",
        _ => return None,
    };
    input.starts_with('\\').then_some((class, 2))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_class_character(input: &str) -> std::result::Result<(char, usize), String> {
    let Some(character) = input.chars().next() else {
        return Err("missing RE2 character class rune".to_string());
    };
    if character != '\\' {
        return Ok((character, character.len_utf8()));
    }
    let Some(escaped) = input[1..].chars().next() else {
        return Err("trailing backslash in RE2 character class".to_string());
    };
    let escaped_len = escaped.len_utf8();
    if escaped.is_ascii() && !escaped.is_ascii_alphanumeric() {
        return Ok((escaped, 1 + escaped_len));
    }
    match escaped {
        '0'..='7' => parse_re2_octal_escape(input, escaped),
        'x' => parse_re2_hex_escape(input),
        'n' => Ok(('\n', 2)),
        'r' => Ok(('\r', 2)),
        't' => Ok(('\t', 2)),
        'a' => Ok(('\u{0007}', 2)),
        'f' => Ok(('\u{000C}', 2)),
        'v' => Ok(('\u{000B}', 2)),
        _ => Err(format!("invalid RE2 character class escape \\{escaped}")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_octal_escape(input: &str, first: char) -> std::result::Result<(char, usize), String> {
    let mut value = first.to_digit(8).expect("octal digit");
    let mut consumed = 2;
    let mut digits = 1;
    for character in input[2..].chars().take(2) {
        let Some(digit) = character.to_digit(8) else {
            break;
        };
        value = value * 8 + digit;
        consumed += character.len_utf8();
        digits += 1;
    }
    if first != '0' && digits == 1 {
        return Err(format!("invalid RE2 octal escape \\{first}"));
    }
    char::from_u32(value)
        .map(|character| (character, consumed))
        .ok_or_else(|| format!("invalid RE2 octal escape value {value:o}"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_hex_escape(input: &str) -> std::result::Result<(char, usize), String> {
    let rest = &input[2..];
    let (digits, consumed) = if let Some(braced) = rest.strip_prefix('{') {
        let Some(end) = braced.find('}') else {
            return Err("unclosed RE2 hexadecimal escape".to_string());
        };
        if end == 0 {
            return Err("empty RE2 hexadecimal escape".to_string());
        }
        (&braced[..end], 3 + end + 1)
    } else {
        if rest.len() < 2 || !rest.is_char_boundary(2) {
            return Err("short RE2 hexadecimal escape".to_string());
        }
        (&rest[..2], 4)
    };
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("invalid RE2 hexadecimal escape {digits}"));
    }
    let value = u32::from_str_radix(digits, 16)
        .map_err(|_| format!("invalid RE2 hexadecimal escape {digits}"))?;
    char::from_u32(value)
        .map(|character| (character, consumed))
        .ok_or_else(|| format!("invalid RE2 hexadecimal escape value {value:X}"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn push_class_rune(output: &mut String, character: char) {
    use std::fmt::Write;

    write!(output, "\\x{{{:X}}}", character as u32).expect("writing to a String cannot fail");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn next_character(input: &str, cursor: usize) -> char {
    input[cursor..]
        .chars()
        .next()
        .expect("cursor must point inside input")
}

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const OUTPUT_SEPARATOR: &str =
    "================================================================================";

struct Scratch(PathBuf);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Scratch {
    fn create() -> Result<Self> {
        let id = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("duckdb-rust-sqllogic-{}-{id}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path.canonicalize()?))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compatibility entry point used by repository component suites. A file with
/// skipped records is deliberately not returned as a fully passing file.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(dead_code)]
pub fn run_file(database: &Database, path: &Path) -> Result<usize> {
    let report = run_file_report(database, path)?;
    match report.status {
        FileStatus::Passed => Ok(report.passed),
        FileStatus::Skipped(reason) | FileStatus::GeneratedOutput(reason) => Err(Error::Execution(
            format!("{} was not fully executed: {reason}", path.display()),
        )),
    }
}

/// Execute one file through the byte parser, source-rooted fixture resolver,
/// and typed result oracle. Wave B owns loop scheduling, restart/load and
/// concurrent controls; encountering one here remains a visible harness error.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn run_file_report(database: &Database, path: &Path) -> Result<FileReport> {
    let path = path.canonicalize()?;
    let source = std::fs::read(&path)?;
    let source_root = source_root_for(&path)?;
    let scratch = Scratch::create()?;
    let mut fixtures = FixtureResolver::new(&source_root, &scratch.0).map_err(fixture_error)?;
    let top = fixtures.begin_script(&path).map_err(fixture_error)?;
    let expected_files = SourceRootFileResolver::new(&source_root).map_err(Error::Execution)?;
    let substitutions = Substitutions::default();
    substitutions.insert("SOURCE_DIR", source_root.as_os_str().as_encoded_bytes());
    substitutions.insert("TEST_DIR", scratch.0.as_os_str().as_encoded_bytes());
    substitutions.insert(
        "TEST_DIR_ABSOLUTE",
        scratch.0.as_os_str().as_encoded_bytes(),
    );
    for (name, value) in std::env::vars_os() {
        substitutions.insert(name.as_encoded_bytes(), value.as_encoded_bytes());
    }
    let matcher = RustRe2Matcher;
    let mut oracle = Oracle::new()
        .with_regex(&matcher)
        .with_substitutions(&substitutions)
        .with_files(&expected_files);
    let mut parser = SqlLogicParser::from_bytes(&path, &source);
    let mut accounting = RecordAccounting::default();
    let mut connections = BTreeMap::<String, Connection>::new();
    let mut directives = default_directive_state();
    let mut hash_threshold = 0usize;
    let mut output = Vec::new();
    let mut generated = 0usize;
    let mut active_sources = vec![top];
    let original_sqlite = path
        .to_string_lossy()
        .contains("third_party/sqllogictest/test/");
    // The repository's pre-port corpus predates DuckDB's mandatory expected-
    // error text. Keep that local compatibility input readable while pinned
    // upstream `test/sql` files use the strict development-runner contract.
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    let allow_missing_error = original_sqlite
        || (source_root == repository_root && path.starts_with(source_root.join("test/sql")));

    while let Some(start) = parser.next_statement().map_err(parse_error)? {
        sync_include_stack(&mut fixtures, &mut active_sources, &start.location)?;
        let mut token = parser.tokenize().map_err(parse_error)?;
        if token.kind.is_single_line() && !parser.next_line_empty_or_comment() {
            return Err(at(
                &token,
                "all test statements need to be separated by an empty line",
            ));
        }

        let mut skip_record = false;
        while matches!(token.kind, TokenKind::SkipIf | TokenKind::OnlyIf) {
            let skip_if = token.kind == TokenKind::SkipIf;
            let system = token
                .parameters
                .first()
                .ok_or_else(|| at(&token, "skipif/onlyif requires a parameter"))?;
            let system = ascii(system, &token, "condition")?.to_ascii_lowercase();
            if system.contains('=') && !original_sqlite {
                return Err(at(
                    &token,
                    "loop-variable conditions require the Wave B loop runner",
                ));
            }
            let ours = system == "duckdb" || (original_sqlite && system == "postgresql");
            skip_record |= ours == skip_if;
            parser.next_line();
            token = parser.tokenize().map_err(parse_error)?;
        }

        if skip_record || (directives.mode.skip_depth > 0 && token.kind != TokenKind::Mode) {
            if token.kind.is_test_command() {
                let declaration = accounting.declare(token.location.clone());
                let execution = accounting
                    .plan(declaration, Vec::new())
                    .map_err(accounting_error)?;
                accounting
                    .record(execution, ExecutionOutcome::Skipped)
                    .map_err(accounting_error)?;
            }
            continue;
        }
        if token.kind.is_test_command() {
            directives.mark_test_command();
        }

        match token.kind {
            TokenKind::Statement => {
                let statement_debug = token.parameters.first().is_some_and(|argument| {
                    matches!(argument.as_slice(), b"debug" | b"debug_skip")
                });
                let debug_skip = execute_statement(
                    database,
                    &mut connections,
                    &mut parser,
                    &mut accounting,
                    &mut oracle,
                    &substitutions,
                    &token,
                    allow_missing_error,
                    directives.mode,
                    &mut output,
                )?;
                if debug_skip {
                    directives.mode.skip_depth += 1;
                }
                generated += usize::from(statement_debug);
            }
            TokenKind::Query => {
                execute_query(
                    database,
                    &mut connections,
                    &mut parser,
                    &mut accounting,
                    &mut oracle,
                    &substitutions,
                    &token,
                    hash_threshold,
                    original_sqlite,
                    directives.mode,
                    &mut output,
                )?;
                generated += usize::from(directives.mode.output_hash);
            }
            TokenKind::HashThreshold => {
                let value = one_argument(&token, "hash-threshold")?;
                hash_threshold = ascii(value, &token, "hash threshold")?
                    .parse()
                    .map_err(|_| at(&token, "hash-threshold must be a non-negative number"))?;
            }
            TokenKind::Halt => break,
            TokenKind::Reset => {
                let args = string_arguments(&token)?;
                if args.len() != 2 || !args[0].eq_ignore_ascii_case("label") {
                    return Err(at(&token, "expected reset label NAME"));
                }
                oracle
                    .reset_label(&args[1])
                    .map_err(|error| at(&token, &error.to_string()))?;
            }
            TokenKind::Set => apply_set(&token, &substitutions)?,
            TokenKind::Unzip => {
                let args = string_arguments(&token)?;
                if !(args.len() == 1 || args.len() == 2) {
                    return Err(at(&token, "unzip requires one input and optional output"));
                }
                let source = substitutions.replace(args[0].as_bytes());
                let source = std::str::from_utf8(&source)
                    .map_err(|_| at(&token, "unzip source is not UTF-8"))?;
                let output = args
                    .get(1)
                    .map(|value| substitutions.replace(value.as_bytes()))
                    .transpose_utf8(&token, "unzip output")?;
                let output = output.as_deref().filter(|output| *output != "NULL");
                fixtures
                    .unzip(source, output.map(Path::new))
                    .map_err(fixture_error)?;
            }
            TokenKind::Mode
            | TokenKind::Require
            | TokenKind::RequireEnv
            | TokenKind::TestEnv
            | TokenKind::Tags
            | TokenKind::Sleep
            | TokenKind::Continue
            | TokenKind::Include => {
                let header = directive_header(&token)?;
                match directives.evaluate(&header) {
                    DirectiveAction::None
                    | DirectiveAction::AddTags(_)
                    | DirectiveAction::SetMode(_) => {}
                    DirectiveAction::SetEnvironment { name, value } => {
                        substitutions.insert(name, value);
                    }
                    DirectiveAction::Sleep(duration) => std::thread::sleep(duration),
                    DirectiveAction::ContinueLoop => {
                        return Err(at(&token, "continue requires the Wave B loop runner"));
                    }
                    DirectiveAction::SkipFile { reason } => {
                        let snapshot = accounting.snapshot();
                        fixtures.finish_script();
                        return Ok(FileReport {
                            status: FileStatus::Skipped(reason),
                            declarations: snapshot.declarations,
                            passed: snapshot.passed.saturating_sub(generated),
                            skipped: snapshot.skipped,
                            generated,
                            output,
                        });
                    }
                    DirectiveAction::Fail { message } => return Err(Error::Execution(message)),
                    DirectiveAction::Include {
                        path: include,
                        location,
                    } => {
                        let current = active_sources
                            .last()
                            .ok_or_else(|| Error::Internal("empty include stack".into()))?;
                        let include_path = fixtures
                            .enter_include(current, &include, location)
                            .map_err(fixture_error)?;
                        let bytes = std::fs::read(&include_path)?;
                        parser.push_include(&include_path, bytes);
                        active_sources.push(include_path);
                    }
                }
            }
            TokenKind::SkipIf | TokenKind::OnlyIf => unreachable!("conditions consumed above"),
            TokenKind::Loop
            | TokenKind::Foreach
            | TokenKind::ConcurrentLoop
            | TokenKind::ConcurrentForeach
            | TokenKind::EndLoop
            | TokenKind::Load
            | TokenKind::Restart
            | TokenKind::Reconnect => {
                return Err(at(
                    &token,
                    "directive requires the Wave B session/concurrent runner",
                ));
            }
            TokenKind::Invalid => return Err(at(&token, "invalid SQLLogicTest directive")),
        }
    }
    fixtures.finish_script();
    let snapshot = accounting.snapshot();
    if snapshot.declarations == 0 || snapshot.pending != 0 {
        return Err(Error::Execution(format!(
            "{} contains no completely accounted test records",
            path.display()
        )));
    }
    if snapshot.failed != 0 || snapshot.unreached != 0 {
        return Err(Error::Execution(format!(
            "{} has failed or unreached records",
            path.display()
        )));
    }
    Ok(FileReport {
        status: if snapshot.skipped != 0 {
            FileStatus::Skipped(format!("{} records skipped", snapshot.skipped))
        } else if generated != 0 {
            FileStatus::GeneratedOutput(format!(
                "{generated} record(s) emitted unchecked generated output"
            ))
        } else {
            FileStatus::Passed
        },
        declarations: snapshot.declarations,
        passed: snapshot.passed.saturating_sub(generated),
        skipped: snapshot.skipped,
        generated,
        output,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_statement(
    database: &Database,
    connections: &mut BTreeMap<String, Connection>,
    parser: &mut SqlLogicParser,
    accounting: &mut RecordAccounting,
    oracle: &mut Oracle<'_>,
    substitutions: &Substitutions,
    token: &Token,
    allow_missing_error: bool,
    mode: Mode,
    output: &mut Vec<String>,
) -> Result<bool> {
    let args = string_arguments(token)?;
    let statement_debug = matches!(
        args.first().map(String::as_str),
        Some("debug" | "debug_skip")
    );
    let debug_skip = args.first().is_some_and(|arg| arg == "debug_skip");
    let expected = match args.first().map(String::as_str) {
        Some("ok") => ExpectedStatement::Success,
        Some("error") => ExpectedStatement::Error(None),
        Some("maybe") => ExpectedStatement::Unknown(None),
        Some("debug" | "debug_skip") => ExpectedStatement::DontCare,
        _ => {
            return Err(at(
                token,
                "statement argument must be ok, error, maybe or debug",
            ));
        }
    };
    let connection_name = args.get(1).cloned().unwrap_or_default();
    parser.next_line();
    let sql = parser.extract_statement();
    if sql.bytes.is_empty() {
        return Err(at(token, "unexpected empty statement text"));
    }
    let expects_message = matches!(
        expected,
        ExpectedStatement::Error(_) | ExpectedStatement::Unknown(_)
    );
    let error_section = parser
        .extract_expected_error(expects_message, allow_missing_error)
        .map_err(parse_error)?;
    let expected = match expected {
        ExpectedStatement::Error(_) => ExpectedStatement::Error(
            (!error_section.bytes.is_empty()).then_some(error_section.bytes.as_slice()),
        ),
        ExpectedStatement::Unknown(_) => ExpectedStatement::Unknown(
            (!error_section.bytes.is_empty()).then_some(error_section.bytes.as_slice()),
        ),
        other => other,
    };
    let declaration = accounting.declare(token.location.clone());
    let execution = accounting
        .plan(declaration, Vec::new())
        .map_err(accounting_error)?;
    let sql_bytes = substitutions.replace(&sql.bytes);
    if mode.output_result || mode.debug || statement_debug {
        append_output_preamble(output, token, &sql_bytes);
    }
    let text = transport_sql(&sql_bytes).map_err(|error| {
        let _ = accounting.record(execution, ExecutionOutcome::Failed);
        at(token, &format!("{error}; SQL {}", escaped(&sql_bytes)))
    })?;
    let outcome = connections
        .entry(connection_name.clone())
        .or_insert_with(|| database.connect())
        .execute(text);
    if mode.output_result || mode.debug || statement_debug {
        match &outcome {
            Ok(results) => {
                for result in results {
                    let values =
                        convert_output_result(result, false).map_err(|error| at(token, &error))?;
                    append_output_result(output, result, &values);
                }
            }
            Err(error) => output.push(format!("error: {error}")),
        }
    }
    let checked = match outcome {
        Ok(_) => oracle.check_statement(StatementResult::Success, expected),
        Err(error) => {
            let message = error.to_string();
            oracle.check_statement(
                StatementResult::Error(ActualError {
                    message: message.as_bytes(),
                    rendered: message.as_bytes(),
                    kind: error_kind(&error),
                }),
                expected,
            )
        }
    };
    if let Err(error) = checked {
        accounting
            .record(execution, ExecutionOutcome::Failed)
            .map_err(accounting_error)?;
        return Err(at(token, &format!("{error}; SQL {}", escaped(&sql_bytes))));
    }
    accounting
        .record(execution, ExecutionOutcome::Passed)
        .map_err(accounting_error)?;
    Ok(debug_skip)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_query(
    database: &Database,
    connections: &mut BTreeMap<String, Connection>,
    parser: &mut SqlLogicParser,
    accounting: &mut RecordAccounting,
    oracle: &mut Oracle<'_>,
    substitutions: &Substitutions,
    token: &Token,
    hash_threshold: usize,
    original_sqlite: bool,
    mode: Mode,
    output: &mut Vec<String>,
) -> Result<()> {
    let args = string_arguments(token)?;
    let signature = args
        .first()
        .ok_or_else(|| at(token, "query requires an I/R/T signature"))?;
    if signature.is_empty()
        || !signature
            .bytes()
            .all(|byte| matches!(byte, b'I' | b'R' | b'T'))
    {
        return Err(at(token, "query signature must contain only I, R or T"));
    }
    let mut sort = SortMode::None;
    let mut connection_name = String::new();
    if let Some(second) = args.get(1) {
        if let Some(parsed) = sort_mode(second) {
            sort = parsed;
        } else {
            connection_name = second.clone();
        }
    }
    let label = args.get(2).map(String::as_str);
    parser.next_line();
    let sql = parser.extract_statement();
    let expected_lines = parser.extract_expected_result();
    let expected: Vec<_> = expected_lines
        .iter()
        .map(|line| line.normalized())
        .collect();
    let declaration = accounting.declare(token.location.clone());
    let execution = accounting
        .plan(declaration, Vec::new())
        .map_err(accounting_error)?;
    let sql_bytes = substitutions.replace(&sql.bytes);
    if mode.output_result || mode.debug {
        append_output_preamble(output, token, &sql_bytes);
    }
    let text = transport_sql(&sql_bytes).map_err(|error| {
        let _ = accounting.record(execution, ExecutionOutcome::Failed);
        at(token, &format!("{error}; SQL {}", escaped(&sql_bytes)))
    })?;
    let result = connections
        .entry(connection_name)
        .or_insert_with(|| database.connect())
        .query(text)
        .map_err(|error| {
            let _ = accounting.record(execution, ExecutionOutcome::Failed);
            at(token, &format!("{error}; SQL {}", escaped(&sql_bytes)))
        })?;
    let logical_types: Vec<_> = result
        .columns
        .iter()
        .map(|column| column.data_type.to_string())
        .collect();
    let columns: Vec<_> = result
        .columns
        .iter()
        .zip(&logical_types)
        .map(|(column, logical_type)| ActualColumn {
            name: &column.name,
            logical_type,
        })
        .collect();
    let rendered: Vec<_> = result
        .rows
        .iter()
        .flatten()
        .map(|value| match value {
            Value::Null => None,
            _ => Some(value.to_string()),
        })
        .collect();
    let cells: Vec<_> = rendered
        .iter()
        .map(|value| match value {
            None => ActualCell::Null,
            Some(value) => ActualCell::Text(value),
        })
        .collect();
    let actual_values = convert_output_result(&result, original_sqlite).map_err(|error| {
        let _ = accounting.record(execution, ExecutionOutcome::Failed);
        at(token, &error)
    })?;
    if mode.output_result {
        append_output_result(output, &result, &actual_values);
    }
    if mode.output_hash {
        let mut hash_values = actual_values.clone();
        sort_output_values(sort, &mut hash_values, result.columns.len())?;
        output.push(OUTPUT_SEPARATOR.into());
        output.push(sql_with_semicolon(&sql_bytes));
        output.push(OUTPUT_SEPARATOR.into());
        output.push(output_hash(&hash_values));
        output.push(OUTPUT_SEPARATOR.into());
        accounting
            .record(execution, ExecutionOutcome::Passed)
            .map_err(accounting_error)?;
        return Ok(());
    }
    let checked = oracle.check_query(
        ActualResult {
            columns: &columns,
            row_count: result.rows.len(),
            cells: &cells,
        },
        QueryExpectation {
            expected_column_count: signature.len(),
            values: ExpectedValues::Lines(&expected),
            sort,
            fallback_sort: SortMode::None,
            label,
            hash_threshold,
            original_sqlite_test: original_sqlite,
        },
    );
    if let Err(error) = checked {
        accounting
            .record(execution, ExecutionOutcome::Failed)
            .map_err(accounting_error)?;
        return Err(at(token, &format!("{error}; SQL {}", escaped(&sql_bytes))));
    }
    accounting
        .record(execution, ExecutionOutcome::Passed)
        .map_err(accounting_error)
}

/// Convert in row-major order using the pinned runner's
/// `SQLLogicTestConvertValue` wire format.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn convert_output_result(
    result: &QueryResult,
    original_sqlite: bool,
) -> std::result::Result<Vec<String>, String> {
    result
        .rows
        .iter()
        .flat_map(|row| row.iter().enumerate())
        .map(|(column, value)| {
            let data_type = &result.columns[column].data_type;
            convert_output_value(value, data_type, original_sqlite)
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn convert_output_value(
    value: &Value,
    data_type: &DataType,
    original_sqlite: bool,
) -> std::result::Result<String, String> {
    if matches!(value, Value::Null) {
        return Ok("NULL".into());
    }
    if original_sqlite {
        let integer = match (data_type, value) {
            (DataType::Float, Value::Float(value)) => {
                rounded_sqlite_float(f64::from(*value), "FLOAT")?
            }
            (DataType::Double, Value::Double(value)) => rounded_sqlite_float(*value, "DOUBLE")?,
            (DataType::Decimal { scale, .. }, Value::Decimal { value, .. }) => {
                let divisor = 10_u128.pow(u32::from(*scale));
                let magnitude = value.unsigned_abs();
                let quotient = magnitude / divisor;
                let remainder = magnitude % divisor;
                let rounded = quotient + u128::from(remainder.saturating_mul(2) >= divisor);
                let signed = i128::try_from(rounded)
                    .ok()
                    .and_then(|rounded| {
                        if *value < 0 {
                            rounded.checked_neg()
                        } else {
                            Some(rounded)
                        }
                    })
                    .ok_or_else(|| "DECIMAL cannot be cast to BIGINT".to_string())?;
                i64::try_from(signed)
                    .map_err(|_| "DECIMAL cannot be cast to BIGINT".to_string())?
                    .to_string()
            }
            _ => String::new(),
        };
        if !integer.is_empty() {
            return Ok(integer);
        }
    }
    if let (DataType::Boolean, Value::Boolean(value)) = (data_type, value) {
        return Ok(if *value { "1" } else { "0" }.into());
    }
    let rendered = value.to_string();
    if rendered.is_empty() {
        Ok("(empty)".into())
    } else {
        Ok(rendered.replace('\0', "\\0"))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn rounded_sqlite_float(value: f64, name: &str) -> std::result::Result<String, String> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded > i64::MAX as f64 {
        return Err(format!("{name} cannot be cast to BIGINT"));
    }
    Ok((rounded as i64).to_string())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_output_result(output: &mut Vec<String>, result: &QueryResult, values: &[String]) {
    if result.columns.is_empty() {
        output.push(format!("{} affected row(s)", result.affected_rows));
        return;
    }
    output.push(
        result
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>()
            .join("\t"),
    );
    output.push(
        result
            .columns
            .iter()
            .map(|column| column.data_type.to_string())
            .collect::<Vec<_>>()
            .join("\t"),
    );
    output.push(OUTPUT_SEPARATOR.into());
    for row in values.chunks(result.columns.len()) {
        output.push(row.join("\t"));
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_output_preamble(output: &mut Vec<String>, token: &Token, sql: &[u8]) {
    output.push(OUTPUT_SEPARATOR.into());
    output.push(format!("File {})", token.location));
    output.push("SQL Query".into());
    output.push(String::from_utf8_lossy(sql).into_owned());
    output.push(OUTPUT_SEPARATOR.into());
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sql_with_semicolon(sql: &[u8]) -> String {
    let mut sql = String::from_utf8_lossy(sql).into_owned();
    if sql.ends_with('\n') {
        if !sql.ends_with(";\n") {
            sql.pop();
            sql.push_str(";\n");
        }
    } else if !sql.ends_with(';') {
        sql.push(';');
    }
    sql
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sort_output_values(mode: SortMode, values: &mut [String], columns: usize) -> Result<()> {
    match mode {
        SortMode::None => Ok(()),
        SortMode::Values => {
            values.sort();
            Ok(())
        }
        SortMode::Rows => {
            if columns == 0 || !values.len().is_multiple_of(columns) {
                return Err(Error::Internal(format!(
                    "cannot row-sort {} generated values into {columns} columns",
                    values.len()
                )));
            }
            let mut rows: Vec<Vec<String>> =
                values.chunks(columns).map(<[String]>::to_vec).collect();
            rows.sort();
            for (target, value) in values.iter_mut().zip(rows.into_iter().flatten()) {
                *target = value;
            }
            Ok(())
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn output_hash(values: &[String]) -> String {
    let mut md5 = Md5::new();
    for value in values {
        md5.update(value.as_bytes());
        md5.update(b"\n");
    }
    format!("{} values hashing to {}", values.len(), md5.finish_hex())
}

// Small local MD5 implementation matching DuckDB's `MD5Context::FinishHex()`.
// The result oracle has the same primitive, but its state is intentionally
// private; output generation must compute the digest before deciding whether
// expectation comparison is applicable.
struct Md5 {
    state: [u32; 4],
    length: u64,
    pending: Vec<u8>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Md5 {
    fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            length: 0,
            pending: Vec::new(),
        }
    }

    fn update(&mut self, input: &[u8]) {
        self.length = self.length.wrapping_add(input.len() as u64);
        self.pending.extend_from_slice(input);
        while self.pending.len() >= 64 {
            let block: [u8; 64] = self.pending[..64].try_into().expect("block length");
            self.compress(&block);
            self.pending.drain(..64);
        }
    }

    fn finish_hex(mut self) -> String {
        let bit_length = self.length.wrapping_mul(8);
        self.pending.push(0x80);
        while self.pending.len() % 64 != 56 {
            self.pending.push(0);
        }
        self.pending.extend_from_slice(&bit_length.to_le_bytes());
        while !self.pending.is_empty() {
            let block: [u8; 64] = self.pending[..64].try_into().expect("block length");
            self.compress(&block);
            self.pending.drain(..64);
        }
        let mut result = String::with_capacity(32);
        for word in self.state {
            for byte in word.to_le_bytes() {
                use std::fmt::Write;
                write!(result, "{byte:02x}").expect("writing to String cannot fail");
            }
        }
        result
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const SHIFT: [u32; 64] = [
            7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
            5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
            6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
        ];
        const K: [u32; 64] = [
            0xd76a_a478,
            0xe8c7_b756,
            0x2420_70db,
            0xc1bd_ceee,
            0xf57c_0faf,
            0x4787_c62a,
            0xa830_4613,
            0xfd46_9501,
            0x6980_98d8,
            0x8b44_f7af,
            0xffff_5bb1,
            0x895c_d7be,
            0x6b90_1122,
            0xfd98_7193,
            0xa679_438e,
            0x49b4_0821,
            0xf61e_2562,
            0xc040_b340,
            0x265e_5a51,
            0xe9b6_c7aa,
            0xd62f_105d,
            0x0244_1453,
            0xd8a1_e681,
            0xe7d3_fbc8,
            0x21e1_cde6,
            0xc337_07d6,
            0xf4d5_0d87,
            0x455a_14ed,
            0xa9e3_e905,
            0xfcef_a3f8,
            0x676f_02d9,
            0x8d2a_4c8a,
            0xfffa_3942,
            0x8771_f681,
            0x6d9d_6122,
            0xfde5_380c,
            0xa4be_ea44,
            0x4bde_cfa9,
            0xf6bb_4b60,
            0xbebf_bc70,
            0x289b_7ec6,
            0xeaa1_27fa,
            0xd4ef_3085,
            0x0488_1d05,
            0xd9d4_d039,
            0xe6db_99e5,
            0x1fa2_7cf8,
            0xc4ac_5665,
            0xf429_2244,
            0x432a_ff97,
            0xab94_23a7,
            0xfc93_a039,
            0x655b_59c3,
            0x8f0c_cc92,
            0xffef_f47d,
            0x8584_5dd1,
            0x6fa8_7e4f,
            0xfe2c_e6e0,
            0xa301_4314,
            0x4e08_11a1,
            0xf753_7e82,
            0xbd3a_f235,
            0x2ad7_d2bb,
            0xeb86_d391,
        ];
        let mut words = [0_u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes(block[index * 4..index * 4 + 4].try_into().expect("word"));
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for index in 0..64 {
            let (function, word) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let next = b.wrapping_add(
                a.wrapping_add(function)
                    .wrapping_add(K[index])
                    .wrapping_add(words[word])
                    .rotate_left(SHIFT[index]),
            );
            a = d;
            d = c;
            c = b;
            b = next;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn default_directive_state() -> DirectiveState {
    let mut state = DirectiveState::default();
    for capability in [
        "64bit",
        "noforcestorage",
        "no_force_storage",
        "nothreadsan",
        "strinline",
        "skip_reload",
        "no_alternative_verify",
        "no_latest_storage",
        "no_vector_verification",
    ] {
        state.available_capabilities.insert(capability.into());
    }
    if cfg!(not(target_os = "windows")) {
        state.available_capabilities.insert("notwindows".into());
    } else {
        state.available_capabilities.insert("windows".into());
    }
    if cfg!(not(target_env = "musl")) {
        state.available_capabilities.insert("notmusl".into());
    }
    if cfg!(not(target_os = "windows")) || cfg!(not(target_env = "gnu")) {
        state.available_capabilities.insert("notmingw".into());
    }
    state.configured_environment = std::env::vars()
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    state.environment = std::env::vars().collect();
    state
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn directive_header(token: &Token) -> Result<Header> {
    Ok(Header {
        location: directives::SourceLocation {
            source: token.location.source.display().to_string(),
            line: token.location.line,
        },
        keyword: token_keyword(token.kind).to_string(),
        arguments: string_arguments(token)?,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn token_keyword(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Mode => "mode",
        TokenKind::Require => "require",
        TokenKind::RequireEnv => "require-env",
        TokenKind::TestEnv => "test-env",
        TokenKind::Tags => "tags",
        TokenKind::Sleep => "sleep",
        TokenKind::Continue => "continue",
        TokenKind::Include => "include",
        _ => "",
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn apply_set(token: &Token, substitutions: &Substitutions) -> Result<()> {
    let args = string_arguments(token)?;
    match args.as_slice() {
        [kind, name, value] if kind == "variable" => {
            substitutions.insert(name, value);
            Ok(())
        }
        [kind, ..] if kind == "ignore_error_messages" || kind == "always_fail_error_messages" => {
            Err(at(
                token,
                "error-message sets require the Wave B runner state",
            ))
        }
        [kind, ..] if kind == "seed" => Err(at(token, "seed requires Wave B runner support")),
        _ => Err(at(token, "unrecognized set parameter")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sync_include_stack(
    fixtures: &mut FixtureResolver,
    active: &mut Vec<PathBuf>,
    location: &parser::SourceLocation,
) -> Result<()> {
    let desired = location.include_sites.len() + 1;
    while active.len() > desired {
        let completed = active
            .pop()
            .ok_or_else(|| Error::Internal("empty include stack".into()))?;
        fixtures.leave_include(&completed);
    }
    if active.last().map(PathBuf::as_path) != Some(location.source.as_path()) {
        return Err(Error::Internal(format!(
            "parser/fixture include stacks disagree at {location}"
        )));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn source_root_for(path: &Path) -> Result<PathBuf> {
    if let Some(root) = path
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
    {
        return Ok(root.to_path_buf());
    }
    for ancestor in path.ancestors() {
        let test = ancestor.join("test");
        if test.is_dir() && path.starts_with(&test) {
            return Ok(ancestor.to_path_buf());
        }
    }
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::Execution(format!("{} has no source root", path.display())))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn error_kind(error: &Error) -> ErrorKind {
    match error {
        Error::Unsupported(_) => ErrorKind::Unsupported,
        Error::Internal(_) => ErrorKind::Internal,
        Error::Execution(message) if message.contains("unoptimized result differs") => {
            ErrorKind::Verification
        }
        _ => ErrorKind::Regular,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Adapt byte-preserving SQLLogic input to the current Rust engine API.
///
/// `Connection` accepts `&str`, so invalid UTF-8 cannot cross this transport.
/// This is a harness limitation, not an engine parser diagnostic, and must not
/// satisfy an expected SQL error.
fn transport_sql(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|error| {
        let position = error.valid_up_to();
        Error::Unsupported(format!(
            "SQLLogic transport accepts only UTF-8 SQL; invalid byte at offset {position}; engine was not invoked"
        ))
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sort_mode(value: &str) -> Option<SortMode> {
    match value {
        "nosort" | "none" => Some(SortMode::None),
        "rowsort" => Some(SortMode::Rows),
        "valuesort" => Some(SortMode::Values),
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn one_argument<'a>(token: &'a Token, name: &str) -> Result<&'a [u8]> {
    if token.parameters.len() != 1 {
        return Err(at(token, &format!("{name} requires one argument")));
    }
    Ok(&token.parameters[0])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn string_arguments(token: &Token) -> Result<Vec<String>> {
    token
        .parameters
        .iter()
        .map(|argument| ascii(argument, token, "directive argument").map(str::to_string))
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ascii<'a>(bytes: &'a [u8], token: &Token, role: &str) -> Result<&'a str> {
    if !bytes.is_ascii() {
        return Err(at(token, &format!("{role} is not ASCII")));
    }
    std::str::from_utf8(bytes).map_err(|_| at(token, &format!("{role} is not UTF-8")))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn at(token: &Token, message: &str) -> Error {
    Error::Execution(format!("{}: {message}", token.location))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_error(error: parser::ParseError) -> Error {
    Error::Execution(error.to_string())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixture_error(error: fixtures::FixtureError) -> Error {
    Error::Execution(error.to_string())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn accounting_error(error: &'static str) -> Error {
    Error::Internal(error.into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn escaped(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn replace_all(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return input.to_vec();
    }
    let mut result = Vec::with_capacity(input.len());
    let mut position = 0;
    while let Some(offset) = input[position..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let found = position + offset;
        result.extend_from_slice(&input[position..found]);
        result.extend_from_slice(replacement);
        position = found + needle.len();
    }
    result.extend_from_slice(&input[position..]);
    result
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
trait TransposeUtf8 {
    fn transpose_utf8(self, token: &Token, role: &str) -> Result<Option<String>>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TransposeUtf8 for Option<Vec<u8>> {
    fn transpose_utf8(self, token: &Token, role: &str) -> Result<Option<String>> {
        self.map(|value| {
            String::from_utf8(value).map_err(|_| at(token, &format!("{role} is not UTF-8")))
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn substitutions_replace_legacy_marker_before_braced_marker() {
        let substitutions = Substitutions::default();
        substitutions.insert("NAME", "value");
        assert_eq!(substitutions.replace(b"${NAME}/{NAME}"), b"value/value");
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn regex_perl_classes_are_ascii_and_non_ascii_mutations_fail() {
        let matcher = RustRe2Matcher;
        assert!(matcher.full_match(br"\d+", b"123").unwrap());
        assert!(!matcher.full_match(br"\d+", "١٢٣".as_bytes()).unwrap());
        assert!(matcher.full_match(br"\w+", b"word_42").unwrap());
        assert!(!matcher.full_match(br"\w+", "λέξη".as_bytes()).unwrap());
        assert!(matcher.full_match(br"\s+", b" \t\r\n").unwrap());
        assert!(!matcher.full_match(br"\s+", "\u{00a0}".as_bytes()).unwrap());
        assert!(
            matcher
                .full_match("café".as_bytes(), "café".as_bytes())
                .unwrap()
        );
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn regex_character_classes_use_re2_union_and_range_rules() {
        let matcher = RustRe2Matcher;
        for (pattern, matching, non_matching) in [
            ("[a&&b]", vec!["a", "&", "b"], vec!["c"]),
            ("[a~~b]", vec!["a", "~", "b"], vec!["c"]),
            ("[--a]", vec!["-", "0", "a"], vec!["b"]),
            ("[0-9--4]", vec!["-", "/", "0", "9"], vec!["a"]),
            ("[a[b]", vec!["a", "[", "b"], vec!["c"]),
            ("[[]", vec!["["], vec!["]"]),
            ("[]a]", vec!["]", "a"], vec!["["]),
            ("[-a]", vec!["-", "a"], vec!["b"]),
            ("[a-]", vec!["a", "-"], vec!["b"]),
            ("[a-z]", vec!["a", "m", "z"], vec!["A"]),
            ("[[:alpha:]]", vec!["A", "z"], vec!["0"]),
            (r"[\w]", vec!["A", "_", "9"], vec!["é"]),
            (r"[\141-\143]", vec!["a", "b", "c"], vec!["d"]),
            (r"[\x{E9}]", vec!["é"], vec!["e"]),
        ] {
            for value in matching {
                assert!(
                    matcher
                        .full_match(pattern.as_bytes(), value.as_bytes())
                        .unwrap(),
                    "{pattern:?} must match {value:?}"
                );
            }
            for value in non_matching {
                assert!(
                    !matcher
                        .full_match(pattern.as_bytes(), value.as_bytes())
                        .unwrap(),
                    "{pattern:?} must not match {value:?}"
                );
            }
        }

        for pattern in ["[a--b]", r"[a-\d]", r"[\b]"] {
            assert!(
                matcher.full_match(pattern.as_bytes(), b"a").is_err(),
                "invalid RE2 pattern {pattern:?} must fail closed"
            );
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn invalid_utf8_transport_error_states_that_engine_was_not_invoked() {
        let error = transport_sql(b"SELECT \xff").unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));
        assert!(error.to_string().contains("engine was not invoked"));
        assert!(error.to_string().contains("offset 7"));
    }
}
