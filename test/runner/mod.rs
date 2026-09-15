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
use duckdb_rust::{Connection, Database, Error, Result, Value};
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileReport {
    pub status: FileStatus,
    pub declarations: usize,
    pub passed: usize,
    pub skipped: usize,
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
        // Anchoring the expression implements RE2 FullMatch. Both engines use
        // a linear-time automaton and reject look-around and backreferences.
        let expression = format!(r"\A(?:{pattern})\z");
        regex::bytes::RegexBuilder::new(&expression)
            .dot_matches_new_line(true)
            // RE2's Perl character classes are ASCII. In particular, `\d`,
            // `\w`, and `\s` must not grow to Unicode classes in this adapter.
            .unicode(false)
            .build()
            .map(|regex| regex.is_match(value))
            .map_err(|error| error.to_string())
    }
}

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        FileStatus::Skipped(reason) => Err(Error::Execution(format!(
            "{} was not fully executed: {reason}",
            path.display()
        ))),
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
            }
            TokenKind::Query => execute_query(
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
            )?,
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
                            passed: snapshot.passed,
                            skipped: snapshot.skipped,
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
        status: if snapshot.skipped == 0 {
            FileStatus::Passed
        } else {
            FileStatus::Skipped(format!("{} records skipped", snapshot.skipped))
        },
        declarations: snapshot.declarations,
        passed: snapshot.passed,
        skipped: snapshot.skipped,
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
        output.push(format!("{}: SQL {}", token.location, escaped(&sql_bytes)));
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
        let rendered = match &outcome {
            Ok(results) => format!("{} result set(s)", results.len()),
            Err(error) => format!("error: {error}"),
        };
        output.push(format!("{}: RESULT {rendered}", token.location));
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
        output.push(format!("{}: SQL {}", token.location, escaped(&sql_bytes)));
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
    if mode.output_result {
        output.push(format!("{}: RESULT {:?}", token.location, rendered));
    }
    if mode.output_hash {
        output.push(format!(
            "{}: OUTPUT_HASH requested for {} value(s)",
            token.location,
            rendered.len()
        ));
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
    fn invalid_utf8_transport_error_states_that_engine_was_not_invoked() {
        let error = transport_sql(b"SELECT \xff").unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));
        assert!(error.to_string().contains("engine was not invoked"));
        assert!(error.to_string().contains("offset 7"));
    }
}
