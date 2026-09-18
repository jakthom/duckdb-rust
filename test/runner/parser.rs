//! Byte-preserving parser primitives for DuckDB SQLLogicTest files.
//!
//! Directive words are ASCII, but SQL and expected output are not required to
//! be UTF-8.  Keep those sections as bytes until the engine or result oracle
//! consumes them.  This module intentionally does not decide whether SQL bytes
//! are valid: invalid SQL encoding is an engine input, not malformed harness
//! syntax.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LineEnding {
    Lf,
    CrLf,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceLocation {
    pub source: Arc<PathBuf>,
    /// One-based physical line number.
    pub line: usize,
    /// Outermost-to-innermost include sites. The final source and line are in
    /// `source` and `line`; these sites explain how that source was reached.
    pub include_sites: Arc<[IncludeSite]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IncludeSite {
    pub source: Arc<PathBuf>,
    pub line: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source.display(), self.line)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceLine {
    pub location: SourceLocation,
    input: Arc<[u8]>,
    raw_range: Range<usize>,
    /// Present only when C++ compatibility requires removal of a CR byte.
    normalized_with_cr_removed: Option<Arc<[u8]>>,
    pub ending: LineEnding,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SourceLine {
    /// Exact bytes before the LF terminator. For CRLF this includes the CR.
    pub fn raw(&self) -> &[u8] {
        &self.input[self.raw_range.clone()]
    }

    /// C++ SQLLogicParser-compatible line bytes: every CR byte is removed.
    pub fn normalized(&self) -> &[u8] {
        self.normalized_with_cr_removed
            .as_deref()
            .unwrap_or_else(|| self.raw())
    }

    pub fn is_empty(&self) -> bool {
        self.normalized().is_empty()
    }

    pub fn is_comment(&self) -> bool {
        self.normalized().starts_with(b"#")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TriviaKind {
    Blank,
    Comment,
    Header,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Trivia {
    pub kind: TriviaKind,
    pub line: SourceLine,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StatementStart {
    pub location: SourceLocation,
    /// Blank lines, comments, and file headers immediately before this item.
    pub leading_trivia: Vec<Trivia>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TokenKind {
    Invalid,
    SkipIf,
    OnlyIf,
    Statement,
    Query,
    HashThreshold,
    Halt,
    Mode,
    Set,
    Reset,
    Loop,
    Foreach,
    ConcurrentLoop,
    ConcurrentForeach,
    EndLoop,
    Require,
    RequireEnv,
    TestEnv,
    Load,
    Restart,
    Reconnect,
    Sleep,
    Unzip,
    Tags,
    Continue,
    Include,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TokenKind {
    pub fn is_single_line(self) -> bool {
        matches!(
            self,
            Self::HashThreshold
                | Self::Halt
                | Self::Mode
                | Self::Set
                | Self::Reset
                | Self::Loop
                | Self::Foreach
                | Self::ConcurrentLoop
                | Self::ConcurrentForeach
                | Self::EndLoop
                | Self::Require
                | Self::RequireEnv
                | Self::TestEnv
                | Self::Load
                | Self::Restart
                | Self::Reconnect
                | Self::Sleep
                | Self::Unzip
                | Self::Tags
                | Self::Continue
                | Self::Include
        )
    }

    pub fn is_test_command(self) -> bool {
        matches!(self, Self::Statement | Self::Query)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub parameters: Vec<Vec<u8>>,
    pub location: SourceLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ByteSection {
    /// One-based location of the first possible content line. This remains
    /// useful for an empty SQL section.
    pub location: SourceLocation,
    /// C++-compatible content: normalized physical lines joined with LF.
    pub bytes: Vec<u8>,
    /// The physical source lines retain raw CRLF and byte-offset evidence.
    pub lines: Vec<SourceLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParseErrorKind {
    MalformedHarness,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParseError {
    pub kind: ParseErrorKind,
    pub location: SourceLocation,
    pub message: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::error::Error for ParseError {}

/// A source-faithful cursor over one SQLLogicTest source.
///
/// `push_include` is deliberately a content hook. Resolving and authorizing an
/// include path belongs to the fixture adapter; once supplied, parsing follows
/// the C++ parser's include-first/resume-parent behavior.
#[derive(Debug)]
pub(crate) struct SqlLogicParser {
    source: Arc<PathBuf>,
    include_sites: Arc<[IncludeSite]>,
    raw_input: Arc<[u8]>,
    lines: Vec<SourceLine>,
    current_line: usize,
    seen_statement: bool,
    current_include: Option<Box<SqlLogicParser>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SqlLogicParser {
    pub fn from_bytes(source: impl AsRef<Path>, input: impl AsRef<[u8]>) -> Self {
        Self::from_shared_bytes(source, Arc::from(input.as_ref()))
    }

    /// Avoid a second source-sized allocation when the caller owns file bytes.
    pub fn from_owned_bytes(source: impl AsRef<Path>, input: Vec<u8>) -> Self {
        Self::from_shared_bytes(source, Arc::from(input))
    }

    fn from_shared_bytes(source: impl AsRef<Path>, input: Arc<[u8]>) -> Self {
        Self::with_include_sites(
            Arc::new(source.as_ref().to_path_buf()),
            input,
            Arc::from([]),
        )
    }

    fn with_include_sites(
        source: Arc<PathBuf>,
        input: Arc<[u8]>,
        include_sites: Arc<[IncludeSite]>,
    ) -> Self {
        let lines = split_source_lines(source.clone(), input.clone(), include_sites.clone());
        Self {
            source,
            include_sites,
            raw_input: input,
            lines,
            current_line: 0,
            seen_statement: false,
            current_include: None,
        }
    }

    pub fn raw_input(&self) -> &[u8] {
        &self.raw_input
    }

    pub fn source_lines(&self) -> &[SourceLine] {
        &self.lines
    }

    pub fn current_location(&self) -> SourceLocation {
        if let Some(include) = &self.current_include {
            return include.current_location();
        }
        self.location_at(self.current_line)
    }

    /// Install include bytes after the caller has resolved the include token.
    pub fn push_include(&mut self, source: impl AsRef<Path>, input: impl AsRef<[u8]>) {
        if let Some(include) = &mut self.current_include {
            include.push_include(source, input);
            return;
        }
        let site = self.current_location();
        let mut sites = self.include_sites.to_vec();
        sites.push(IncludeSite {
            source: site.source,
            line: site.line,
        });
        self.current_include = Some(Box::new(Self::with_include_sites(
            Arc::new(source.as_ref().to_path_buf()),
            Arc::from(input.as_ref()),
            Arc::from(sites),
        )));
    }

    pub fn next_line_empty_or_comment(&mut self) -> bool {
        if let Some(include) = &mut self.current_include {
            return include.next_line_empty_or_comment();
        }
        self.lines
            .get(self.current_line + 1)
            .is_none_or(empty_or_comment)
    }

    /// Advance to the next directive, retaining skipped comments and blanks.
    pub fn next_statement(&mut self) -> Result<Option<StatementStart>, ParseError> {
        if let Some(include) = &mut self.current_include {
            if let Some(start) = include.next_statement()? {
                return Ok(Some(start));
            }
            self.current_include = None;
        }

        if self.seen_statement {
            while self
                .lines
                .get(self.current_line)
                .is_some_and(|line| !empty_or_comment(line))
            {
                self.current_line += 1;
            }
        }
        self.seen_statement = true;

        let mut leading_trivia = Vec::new();
        while let Some(line) = self.lines.get(self.current_line) {
            if !empty_or_comment(line) {
                break;
            }
            leading_trivia.push(Trivia {
                kind: trivia_kind(line),
                line: line.clone(),
            });
            self.current_line += 1;
        }
        let Some(line) = self.lines.get(self.current_line) else {
            return Ok(None);
        };
        Ok(Some(StatementStart {
            location: line.location.clone(),
            leading_trivia,
        }))
    }

    pub fn next_line(&mut self) {
        if let Some(include) = &mut self.current_include {
            include.next_line();
        } else {
            self.current_line += 1;
        }
    }

    pub fn tokenize(&mut self) -> Result<Token, ParseError> {
        if let Some(include) = &mut self.current_include {
            return include.tokenize();
        }
        let Some(line) = self.lines.get(self.current_line) else {
            return Ok(Token {
                kind: TokenKind::Invalid,
                parameters: Vec::new(),
                location: self.location_at(self.current_line),
            });
        };
        let words = split_ascii_whitespace(line.normalized());
        let Some(command) = words.first() else {
            return Err(self.error_at(
                line.location.clone(),
                ParseErrorKind::MalformedHarness,
                "empty directive line",
            ));
        };
        let kind = command_to_token(command).ok_or_else(|| {
            self.error_at(
                line.location.clone(),
                ParseErrorKind::MalformedHarness,
                format!("unrecognized directive {}", escaped_bytes(command)),
            )
        })?;
        Ok(Token {
            kind,
            parameters: words[1..].iter().map(|word| word.to_vec()).collect(),
            location: line.location.clone(),
        })
    }

    pub fn extract_statement(&mut self) -> ByteSection {
        if let Some(include) = &mut self.current_include {
            return include.extract_statement();
        }
        let location = self.location_at(self.current_line);
        let mut bytes = Vec::new();
        let mut lines = Vec::new();
        while let Some(line) = self.lines.get(self.current_line) {
            if empty_or_comment(line) || line.normalized() == b"----" {
                break;
            }
            if !lines.is_empty() {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(line.normalized());
            lines.push(line.clone());
            self.current_line += 1;
        }
        ByteSection {
            location,
            bytes,
            lines,
        }
    }

    pub fn extract_expected_result(&mut self) -> Vec<SourceLine> {
        if let Some(include) = &mut self.current_include {
            return include.extract_expected_result();
        }
        if self
            .lines
            .get(self.current_line)
            .is_some_and(|line| line.normalized() == b"----")
        {
            self.current_line += 1;
        }
        let mut result = Vec::new();
        while let Some(line) = self.lines.get(self.current_line) {
            // Unlike SQL extraction, a comment is data in an expected section.
            if line.is_empty() {
                break;
            }
            result.push(line.clone());
            self.current_line += 1;
        }
        result
    }

    pub fn extract_expected_error(
        &mut self,
        expects_message: bool,
        original_sqlite_test: bool,
    ) -> Result<ByteSection, ParseError> {
        if let Some(include) = &mut self.current_include {
            return include.extract_expected_error(expects_message, original_sqlite_test);
        }
        let location = self.location_at(self.current_line);
        let has_separator = self
            .lines
            .get(self.current_line)
            .is_some_and(|line| line.normalized() == b"----");
        if !has_separator {
            if expects_message && !original_sqlite_test {
                return Err(self.error_at(
                    location,
                    ParseErrorKind::MalformedHarness,
                    "statement error or maybe requires an expected error section",
                ));
            }
            return Ok(ByteSection {
                location,
                bytes: Vec::new(),
                lines: Vec::new(),
            });
        }
        if !expects_message {
            return Err(self.error_at(
                location,
                ParseErrorKind::MalformedHarness,
                "only statement error or maybe can have an expected error section",
            ));
        }
        self.current_line += 1;
        let location = self.location_at(self.current_line);
        let mut lines = Vec::new();
        let mut bytes = Vec::new();
        while let Some(line) = self.lines.get(self.current_line) {
            if line.is_empty() {
                break;
            }
            if !lines.is_empty() {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(line.normalized());
            lines.push(line.clone());
            self.current_line += 1;
        }
        Ok(ByteSection {
            location,
            bytes,
            lines,
        })
    }

    fn location_at(&self, index: usize) -> SourceLocation {
        SourceLocation {
            source: self.source.clone(),
            line: index + 1,
            include_sites: self.include_sites.clone(),
        }
    }

    fn error_at(
        &self,
        location: SourceLocation,
        kind: ParseErrorKind,
        message: impl Into<String>,
    ) -> ParseError {
        ParseError {
            kind,
            location,
            message: message.into(),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn split_source_lines(
    source: Arc<PathBuf>,
    input: Arc<[u8]>,
    include_sites: Arc<[IncludeSite]>,
) -> Vec<SourceLine> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < input.len() {
        let end = input[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(input.len(), |offset| start + offset);
        let terminated = end < input.len();
        let raw = &input[start..end];
        let ending = if terminated && raw.last() == Some(&b'\r') {
            LineEnding::CrLf
        } else if terminated {
            LineEnding::Lf
        } else {
            LineEnding::None
        };
        let normalized_with_cr_removed = raw.contains(&b'\r').then(|| {
            Arc::from(
                raw.iter()
                    .copied()
                    .filter(|byte| *byte != b'\r')
                    .collect::<Vec<_>>(),
            )
        });
        result.push(SourceLine {
            location: SourceLocation {
                source: source.clone(),
                line: result.len() + 1,
                include_sites: include_sites.clone(),
            },
            input: input.clone(),
            raw_range: start..end,
            normalized_with_cr_removed,
            ending,
        });
        if !terminated {
            break;
        }
        start = end + 1;
    }
    result
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn empty_or_comment(line: &SourceLine) -> bool {
    line.is_empty() || line.is_comment()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn trivia_kind(line: &SourceLine) -> TriviaKind {
    if line.is_empty() {
        TriviaKind::Blank
    } else if line.normalized().starts_with(b"# name:")
        || line.normalized().starts_with(b"# description:")
        || line.normalized().starts_with(b"# group:")
    {
        TriviaKind::Header
    } else {
        TriviaKind::Comment
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn split_ascii_whitespace(line: &[u8]) -> Vec<&[u8]> {
    let mut result = Vec::new();
    let mut start = None;
    for (index, byte) in line.iter().copied().enumerate() {
        if byte.is_ascii_whitespace() {
            if let Some(word_start) = start.take() {
                result.push(&line[word_start..index]);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(word_start) = start {
        result.push(&line[word_start..]);
    }
    result
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn command_to_token(command: &[u8]) -> Option<TokenKind> {
    Some(match command {
        b"skipif" => TokenKind::SkipIf,
        b"onlyif" => TokenKind::OnlyIf,
        b"statement" => TokenKind::Statement,
        b"query" => TokenKind::Query,
        b"hash-threshold" => TokenKind::HashThreshold,
        b"halt" => TokenKind::Halt,
        b"mode" => TokenKind::Mode,
        b"set" => TokenKind::Set,
        b"reset" => TokenKind::Reset,
        b"loop" => TokenKind::Loop,
        b"foreach" => TokenKind::Foreach,
        b"concurrentloop" => TokenKind::ConcurrentLoop,
        b"concurrentforeach" => TokenKind::ConcurrentForeach,
        b"endloop" => TokenKind::EndLoop,
        b"require" => TokenKind::Require,
        b"require-env" => TokenKind::RequireEnv,
        b"test-env" => TokenKind::TestEnv,
        b"load" => TokenKind::Load,
        b"restart" => TokenKind::Restart,
        b"reconnect" => TokenKind::Reconnect,
        b"sleep" => TokenKind::Sleep,
        b"unzip" => TokenKind::Unzip,
        b"tags" => TokenKind::Tags,
        b"continue" => TokenKind::Continue,
        b"include" => TokenKind::Include,
        _ => return None,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn escaped_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct DeclarationId(usize);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ExecutionId {
    declaration: DeclarationId,
    instance: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutionIdentity {
    pub declaration: SourceLocation,
    /// One ordinal per containing loop, outermost first.
    pub loop_iterations: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionOutcome {
    Passed,
    Failed,
    Skipped,
    Unreached,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AccountingSnapshot {
    pub declarations: usize,
    pub planned_executions: usize,
    pub executed: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub unreached: usize,
    /// Planned loop instances for which the runner has not recorded an outcome.
    pub pending: usize,
}

#[derive(Clone, Debug)]
struct ExecutionState {
    identity: ExecutionIdentity,
    outcome: Option<ExecutionOutcome>,
}

#[derive(Clone, Debug)]
struct DeclarationState {
    location: SourceLocation,
    executions: Vec<ExecutionState>,
}

/// Explicit record accounting. Declaring a record, expanding a loop instance,
/// and recording its outcome are intentionally separate operations.
#[derive(Clone, Debug, Default)]
pub(crate) struct RecordAccounting {
    declarations: Vec<DeclarationState>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RecordAccounting {
    pub fn declare(&mut self, location: SourceLocation) -> DeclarationId {
        let id = DeclarationId(self.declarations.len());
        self.declarations.push(DeclarationState {
            location,
            executions: Vec::new(),
        });
        id
    }

    pub fn plan(
        &mut self,
        declaration: DeclarationId,
        loop_iterations: Vec<usize>,
    ) -> Result<ExecutionId, &'static str> {
        let Some(state) = self.declarations.get_mut(declaration.0) else {
            return Err("unknown declaration");
        };
        let instance = state.executions.len();
        state.executions.push(ExecutionState {
            identity: ExecutionIdentity {
                declaration: state.location.clone(),
                loop_iterations,
            },
            outcome: None,
        });
        Ok(ExecutionId {
            declaration,
            instance,
        })
    }

    pub fn identity(&self, execution: ExecutionId) -> Option<&ExecutionIdentity> {
        self.declarations
            .get(execution.declaration.0)?
            .executions
            .get(execution.instance)
            .map(|state| &state.identity)
    }

    pub fn record(
        &mut self,
        execution: ExecutionId,
        outcome: ExecutionOutcome,
    ) -> Result<(), &'static str> {
        let Some(state) = self
            .declarations
            .get_mut(execution.declaration.0)
            .and_then(|declaration| declaration.executions.get_mut(execution.instance))
        else {
            return Err("unknown execution");
        };
        if state.outcome.is_some() {
            return Err("execution outcome already recorded");
        }
        state.outcome = Some(outcome);
        Ok(())
    }

    pub fn snapshot(&self) -> AccountingSnapshot {
        let mut snapshot = AccountingSnapshot {
            declarations: self.declarations.len(),
            ..AccountingSnapshot::default()
        };
        for execution in self
            .declarations
            .iter()
            .flat_map(|declaration| &declaration.executions)
        {
            snapshot.planned_executions += 1;
            match execution.outcome {
                Some(ExecutionOutcome::Passed) => {
                    snapshot.executed += 1;
                    snapshot.passed += 1;
                }
                Some(ExecutionOutcome::Failed) => {
                    snapshot.executed += 1;
                    snapshot.failed += 1;
                }
                Some(ExecutionOutcome::Skipped) => snapshot.skipped += 1,
                Some(ExecutionOutcome::Unreached) => snapshot.unreached += 1,
                None => snapshot.pending += 1,
            }
        }
        snapshot
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ParserWorkloadDigest {
    pub bytes: usize,
    pub records: usize,
    pub checksum: u64,
}

/// Fixed record count for the parser Gate-P comparison against both pins.
pub(crate) const PARSER_BENCHMARK_RECORDS: usize = 4_096;
pub(crate) const PARSER_BENCHMARK_BYTES: usize = 366_470;
pub(crate) const PARSER_BENCHMARK_CHECKSUM: u64 = 17_431_774_223_484_805_419;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Construct the deterministic Gate-P workload. Each record has one directive,
/// two SQL lines, a separator, and two expected lines. The returned bytes can
/// be given unchanged to either pinned C++ SQLLogicParser.
pub(crate) fn build_parser_benchmark_workload(records: usize) -> Vec<u8> {
    let mut input = Vec::with_capacity(records.saturating_mul(72));
    input.extend_from_slice(b"# name: parser-byte-throughput\n# group: [parser]\n\n");
    for index in 0..records {
        input.extend_from_slice(b"query II rowsort parser_bench\nSELECT ");
        input.extend_from_slice(index.to_string().as_bytes());
        input.extend_from_slice(b",\n       42\n----\n");
        input.extend_from_slice(index.to_string().as_bytes());
        input.extend_from_slice(b"\t42\n#expected-comment-byte\n\n");
    }
    input
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Parse and checksum the deterministic Gate-P workload. FNV-1a is updated with
/// each field's eight-byte little-endian length followed by its bytes, in this
/// order: directive kind byte, parameters, SQL, and normalized expected lines.
/// A reference adapter can reproduce this without sharing Rust implementation.
pub(crate) fn digest_parser_benchmark_workload(
    source: &str,
    input: &[u8],
) -> Result<ParserWorkloadDigest, ParseError> {
    let mut parser = SqlLogicParser::from_bytes(source, input);
    let mut checksum = 0xcbf29ce484222325_u64;
    let mut records = 0;
    while parser.next_statement()?.is_some() {
        let token = parser.tokenize()?;
        if token.kind != TokenKind::Query {
            return Err(parser.error_at(
                token.location,
                ParseErrorKind::MalformedHarness,
                "benchmark workload contains a non-query record",
            ));
        }
        checksum_field(&mut checksum, &[token.kind as u8]);
        for parameter in &token.parameters {
            checksum_field(&mut checksum, parameter);
        }
        parser.next_line();
        let sql = parser.extract_statement();
        checksum_field(&mut checksum, &sql.bytes);
        for expected in parser.extract_expected_result() {
            checksum_field(&mut checksum, expected.normalized());
        }
        records += 1;
    }
    Ok(ParserWorkloadDigest {
        bytes: input.len(),
        records,
        checksum,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn checksum_field(checksum: &mut u64, field: &[u8]) {
    for byte in (field.len() as u64).to_le_bytes().iter().chain(field) {
        *checksum ^= u64::from(*byte);
        *checksum = checksum.wrapping_mul(0x100000001b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn parser(source: &str, bytes: &[u8]) -> SqlLogicParser {
        SqlLogicParser::from_bytes(source, bytes)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn pinned_file(checkout: &str, relative: &str) -> (String, Vec<u8>) {
        let path = PathBuf::from("..").join(checkout).join(relative);
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read pinned parser case {}: {error}",
                path.display()
            )
        });
        (path.display().to_string(), bytes)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn development_invalid_utf8_is_engine_sql_with_exact_location() {
        let (path, bytes) = pinned_file(
            "duckdb",
            "test/sql/parser/invalid_utf8_parser_location.test",
        );
        let mut parser = parser(&path, &bytes);
        let start = parser.next_statement().unwrap().unwrap();
        assert_eq!(start.location.source.as_path(), Path::new(&path));
        assert_eq!(start.location.line, 5);
        let token = parser.tokenize().unwrap();
        assert_eq!(token.kind, TokenKind::Statement);
        assert_eq!(token.parameters, [b"error".to_vec()]);
        parser.next_line();
        let sql = parser.extract_statement();
        assert_eq!(sql.location.line, 6);
        assert_eq!(sql.bytes, b"SELECT\n  42\nFR\xF9OM t;");
        assert!(std::str::from_utf8(&sql.bytes).is_err());
        let expected = parser.extract_expected_error(true, false).unwrap();
        assert_eq!(expected.bytes, b"Invalid UTF-8 in query");
        // Invalid SQL encoding was not reclassified as malformed harness input.
        assert_eq!(parser.next_statement().unwrap().unwrap().location.line, 12);
        parser.tokenize().unwrap();
        parser.next_line();
        assert_eq!(
            parser.extract_statement().bytes,
            b"SELECT\n  42\nFR\xF9OM t;"
        );
        assert_eq!(
            parser.extract_expected_error(true, false).unwrap().bytes,
            b"LINE 3:"
        );

        assert_eq!(parser.next_statement().unwrap().unwrap().location.line, 19);
        assert_eq!(parser.tokenize().unwrap().parameters, [b"ok".to_vec()]);
        parser.next_line();
        assert_eq!(
            parser.extract_statement().bytes,
            b"SET errors_as_json=true;"
        );

        assert_eq!(parser.next_statement().unwrap().unwrap().location.line, 22);
        parser.tokenize().unwrap();
        parser.next_line();
        assert_eq!(
            parser.extract_statement().bytes,
            b"SELECT\n  42\nFR\xF9OM t;"
        );
        assert_eq!(
            parser.extract_expected_error(true, false).unwrap().bytes,
            br#""position":"14""#
        );
        assert!(parser.next_statement().unwrap().is_none());
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn both_pins_preserve_invisible_space_bytes_and_syntax_difference() {
        let (development_path, development) =
            pinned_file("duckdb", "test/sql/parser/invisible_spaces.test");
        let (release_path, release) =
            pinned_file("duckdb-v1.5.5", "test/sql/parser/invisible_spaces.test");

        for (path, bytes, expected_foreach_line, placeholder) in [
            (
                development_path,
                development,
                13,
                b"{unicode_space}".as_slice(),
            ),
            (release_path, release, 16, b"${unicode_space}".as_slice()),
        ] {
            let mut parser = parser(&path, &bytes);
            let mut saw_foreach = false;
            let mut saw_unicode_sql = false;
            while let Some(start) = parser.next_statement().unwrap() {
                let token = parser.tokenize().unwrap();
                if token.kind == TokenKind::Foreach {
                    saw_foreach = true;
                    assert_eq!(start.location.line, expected_foreach_line);
                    assert_eq!(token.parameters[0], b"unicode_space");
                    assert!(
                        token
                            .parameters
                            .iter()
                            .skip(1)
                            .any(|value| value == b"\xE2\x80\x80")
                    );
                }
                if token.kind == TokenKind::Query {
                    parser.next_line();
                    let sql = parser.extract_statement();
                    if sql
                        .bytes
                        .windows(placeholder.len())
                        .any(|part| part == placeholder)
                    {
                        saw_unicode_sql = true;
                    }
                    let _ = parser.extract_expected_result();
                }
            }
            assert!(saw_foreach);
            assert!(saw_unicode_sql);
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn crlf_and_interior_cr_follow_cpp_normalization_but_raw_lines_survive() {
        let bytes = b"query\tI\r\nSEL\rECT 42\r\n----\r\n42\r\n\r\n";
        let mut parser = parser("crlf.test", bytes);
        let start = parser.next_statement().unwrap().unwrap();
        assert_eq!(start.location.line, 1);
        let token = parser.tokenize().unwrap();
        assert_eq!(token.parameters, [b"I".to_vec()]);
        assert_eq!(parser.source_lines()[0].ending, LineEnding::CrLf);
        assert_eq!(parser.source_lines()[1].raw(), b"SEL\rECT 42\r");
        parser.next_line();
        assert_eq!(parser.extract_statement().bytes, b"SELECT 42");
        assert_eq!(parser.extract_expected_result()[0].normalized(), b"42");
        assert_eq!(parser.raw_input(), bytes);
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn comments_headers_blank_lines_and_expected_comments_remain_distinct() {
        let mut parser = parser(
            "trivia.test",
            b"# name: x\n# ordinary\n\nquery I\nSELECT 1\n----\n# value, not trivia\n1\n\n",
        );
        let start = parser.next_statement().unwrap().unwrap();
        assert_eq!(
            start
                .leading_trivia
                .iter()
                .map(|item| item.kind)
                .collect::<Vec<_>>(),
            [TriviaKind::Header, TriviaKind::Comment, TriviaKind::Blank]
        );
        parser.tokenize().unwrap();
        parser.next_line();
        parser.extract_statement();
        let expected = parser.extract_expected_result();
        assert_eq!(expected.len(), 2);
        assert_eq!(expected[0].normalized(), b"# value, not trivia");
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn tabs_are_token_separators_but_whitespace_only_lines_are_sql() {
        let mut parser = parser("tabs.test", b"query\tI\trowsort\n \t\n----\nvalue\n");
        parser.next_statement().unwrap().unwrap();
        let token = parser.tokenize().unwrap();
        assert_eq!(token.parameters, [b"I".to_vec(), b"rowsort".to_vec()]);
        assert!(token.kind.is_test_command());
        assert!(!token.kind.is_single_line());
        assert!(!parser.next_line_empty_or_comment());
        parser.next_line();
        assert_eq!(parser.extract_statement().bytes, b" \t");
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn exact_separator_and_single_line_classification_match_cpp() {
        let mut parser = parser("separator.test", b"set x y\n\nquery I\nSELECT 1\n ----\n");
        parser.next_statement().unwrap().unwrap();
        let set = parser.tokenize().unwrap();
        assert!(set.kind.is_single_line());
        assert!(!set.kind.is_test_command());
        assert!(parser.next_line_empty_or_comment());

        parser.next_statement().unwrap().unwrap();
        parser.tokenize().unwrap();
        parser.next_line();
        // A leading space makes this SQL, not the exact four-hyphen separator.
        assert_eq!(parser.extract_statement().bytes, b"SELECT 1\n ----");
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn empty_sql_and_error_sections_are_distinguished() {
        let mut empty = parser("empty.test", b"statement ok\n\n");
        empty.next_statement().unwrap().unwrap();
        empty.tokenize().unwrap();
        empty.next_line();
        let sql = empty.extract_statement();
        assert!(sql.bytes.is_empty());
        assert_eq!(sql.location.line, 2);

        let mut missing = parser("missing-error.test", b"statement error\nSELECT 1\n\n");
        missing.next_statement().unwrap().unwrap();
        missing.tokenize().unwrap();
        missing.next_line();
        missing.extract_statement();
        let error = missing.extract_expected_error(true, false).unwrap_err();
        assert_eq!(error.kind, ParseErrorKind::MalformedHarness);
        assert_eq!(error.location.line, 3);

        let mut present = parser(
            "error.test",
            b"statement error\nSELECT 1\n----\nfirst\nsecond\n\n",
        );
        present.next_statement().unwrap().unwrap();
        present.tokenize().unwrap();
        present.next_line();
        present.extract_statement();
        let error = present.extract_expected_error(true, false).unwrap();
        assert_eq!(error.bytes, b"first\nsecond");
        assert_eq!(error.location.line, 4);
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn include_hook_preserves_context_and_resumes_parent() {
        let mut parser = parser(
            "parent.test",
            b"include child.test\n\nquery I\nSELECT 2\n----\n2\n",
        );
        let include = parser.next_statement().unwrap().unwrap();
        assert_eq!(include.location.line, 1);
        assert_eq!(parser.tokenize().unwrap().kind, TokenKind::Include);
        parser.push_include("child.test", b"query I\nSELECT 1\n----\n1\n");

        let child = parser.next_statement().unwrap().unwrap();
        assert_eq!(child.location.source.as_path(), Path::new("child.test"));
        assert_eq!(child.location.line, 1);
        assert_eq!(
            child.location.include_sites.as_ref(),
            [IncludeSite {
                source: Arc::new(PathBuf::from("parent.test")),
                line: 1,
            }]
        );
        parser.tokenize().unwrap();
        parser.next_line();
        parser.extract_statement();
        parser.extract_expected_result();

        let parent = parser.next_statement().unwrap().unwrap();
        assert_eq!(parent.location.source.as_path(), Path::new("parent.test"));
        assert_eq!(parent.location.line, 3);
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn malformed_directive_is_harness_error_even_when_bytes_are_not_utf8() {
        let mut parser = parser("bad.test", b"bog\xFFus arg\n");
        let start = parser.next_statement().unwrap().unwrap();
        let error = parser.tokenize().unwrap_err();
        assert_eq!(error.kind, ParseErrorKind::MalformedHarness);
        assert_eq!(error.location, start.location);
        assert!(error.message.contains("\\xff"));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn accounting_records_failure_skip_unreached_and_loop_source_identity() {
        let location = SourceLocation {
            source: Arc::new(PathBuf::from("loop.test")),
            line: 8,
            include_sites: Arc::from([]),
        };
        let mut accounting = RecordAccounting::default();
        let declaration = accounting.declare(location.clone());
        let first = accounting.plan(declaration, vec![0]).unwrap();
        let second = accounting.plan(declaration, vec![1]).unwrap();
        let third = accounting.plan(declaration, vec![2]).unwrap();
        let fourth = accounting.plan(declaration, vec![3]).unwrap();
        accounting.record(first, ExecutionOutcome::Passed).unwrap();
        accounting.record(second, ExecutionOutcome::Failed).unwrap();
        accounting.record(third, ExecutionOutcome::Skipped).unwrap();
        accounting
            .record(fourth, ExecutionOutcome::Unreached)
            .unwrap();

        assert_eq!(accounting.identity(second).unwrap().declaration, location);
        assert_eq!(accounting.identity(second).unwrap().loop_iterations, [1]);
        assert_eq!(
            accounting.snapshot(),
            AccountingSnapshot {
                declarations: 1,
                planned_executions: 4,
                executed: 2,
                passed: 1,
                failed: 1,
                skipped: 1,
                unreached: 1,
                pending: 0,
            }
        );
        assert_eq!(
            accounting.record(first, ExecutionOutcome::Failed),
            Err("execution outcome already recorded")
        );
        assert_eq!(accounting.snapshot().passed, 1);
        assert_eq!(accounting.snapshot().failed, 1);
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn zero_selection_and_pending_work_are_not_reported_as_success() {
        let accounting = RecordAccounting::default();
        assert_eq!(accounting.snapshot(), AccountingSnapshot::default());

        let mut accounting = RecordAccounting::default();
        let declaration = accounting.declare(SourceLocation {
            source: Arc::new(PathBuf::from("selected.test")),
            line: 1,
            include_sites: Arc::from([]),
        });
        accounting.plan(declaration, Vec::new()).unwrap();
        assert_eq!(accounting.snapshot().pending, 1);
        assert_eq!(accounting.snapshot().executed, 0);
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn deterministic_benchmark_reports_exact_bytes_records_and_checksum() {
        let input = build_parser_benchmark_workload(PARSER_BENCHMARK_RECORDS);
        let first = digest_parser_benchmark_workload("bench.test", &input).unwrap();
        let second = digest_parser_benchmark_workload("bench.test", &input).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.bytes, input.len());
        assert_eq!(first.records, PARSER_BENCHMARK_RECORDS);
        assert_eq!(first.bytes, PARSER_BENCHMARK_BYTES);
        assert_eq!(first.checksum, PARSER_BENCHMARK_CHECKSUM);
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn owned_source_constructor_keeps_the_same_byte_contract() {
        let bytes = b"query I\nSELECT 1\n----\n1\n".to_vec();
        let parser = SqlLogicParser::from_owned_bytes("owned.test", bytes.clone());
        assert_eq!(parser.raw_input(), bytes);
    }
}
