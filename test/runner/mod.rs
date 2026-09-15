mod directives;
#[allow(dead_code)]
mod fixture_bench;
#[allow(dead_code)]
mod fixtures;
#[allow(dead_code)]
mod oracle;
#[allow(dead_code)]
mod parser;
mod schedule;
mod session;

use directives::{DirectiveAction, DirectiveHeader, DirectiveState, Mode};
use duckdb_rust::{DataType, Database, Error, QueryResult, Result, Value};
use fixtures::FixtureResolver;
use oracle::{
    ActualCell, ActualColumn, ActualError, ActualResult, ErrorKind, ExpectedStatement,
    ExpectedSubstitutions, ExpectedValues, Oracle, QueryExpectation, Re2Matcher, SortMode,
    SourceRootFileResolver, StatementResult,
};
use parser::{
    ByteSection, DeclarationId, ExecutionOutcome, RecordAccounting, SourceLine, SqlLogicParser,
    Token, TokenKind,
};
use schedule::{Condition, LoopDefinition, LoopFrame};
use session::Sessions;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
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
struct Substitutions(RwLock<BTreeMap<Vec<u8>, Vec<u8>>>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Substitutions {
    fn insert(&self, name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.0
            .write()
            .expect("substitution map poisoned")
            .insert(name.as_ref().to_vec(), value.as_ref().to_vec());
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpectedSubstitutions for Substitutions {
    fn replace(&self, input: &[u8]) -> Vec<u8> {
        let mut result = input.to_vec();
        for (name, value) in self.0.read().expect("substitution map poisoned").iter() {
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

/// Validate and translate the pinned RE2 dialect before Rust compilation.
/// This follows `parse.cc`'s main parse loop, `ParsePerlFlags`,
/// `MaybeParseRepetition`, `ParseEscape`, and `ParseCharClass`: only RE2's
/// i/m/s/U flags, group forms, escape inventory, property names and repeat
/// limits enter the generated expression. Literal/runic constructs are then
/// rewritten where Rust has different meanings. Unicode mode stays enabled so
/// `.`, classes and quantifiers retain RE2's UTF-8 codepoint semantics.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn translate_re2_pattern(pattern: &str) -> std::result::Result<String, String> {
    let mut output = String::with_capacity(pattern.len());
    let mut cursor = 0;
    let mut groups = vec![Re2GroupState::default()];
    while cursor < pattern.len() {
        let character = next_character(pattern, cursor);
        match character {
            '\\' => {
                let escape = &pattern[cursor..];
                let Some(escaped) = escape[1..].chars().next() else {
                    return Err("trailing backslash in RE2 pattern".to_string());
                };
                match escaped {
                    // RE2's `\C` consumes one byte even in UTF-8 mode. Keep the
                    // surrounding expression Unicode-aware and disable it only
                    // for this atom.
                    'C' => {
                        begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                        output.push_str("(?s-u:.)");
                        cursor += 2;
                        record_re2_atom(&mut groups, 1);
                    }
                    // In RE2, an unterminated `\Q` quote extends to end of input.
                    // Render each quoted rune as a hex literal so Rust cannot
                    // reinterpret metacharacters, class syntax or free-spacing.
                    'Q' => {
                        cursor += 2;
                        while cursor < pattern.len() {
                            if pattern[cursor..].starts_with("\\E") {
                                cursor += 2;
                                break;
                            }
                            let quoted = next_character(pattern, cursor);
                            begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                            push_regex_rune(&mut output, quoted as u32);
                            cursor += quoted.len_utf8();
                            record_re2_atom(&mut groups, 1);
                        }
                    }
                    'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                        let (class, consumed) = re2_perl_class(escape).expect("known Perl class");
                        begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                        output.push_str(class);
                        cursor += consumed;
                        record_re2_atom(&mut groups, 1);
                    }
                    // RE2's Perl word boundaries use the ASCII `\w` definition.
                    'b' | 'B' => {
                        begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                        output.push_str("(?-u:\\");
                        output.push(escaped);
                        output.push(')');
                        cursor += 2;
                        record_re2_atom(&mut groups, 1);
                    }
                    'A' | 'z' => {
                        begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                        output.push('\\');
                        output.push(escaped);
                        cursor += 2;
                        record_re2_atom(&mut groups, 1);
                    }
                    'p' | 'P' => {
                        let (class, consumed) = parse_re2_unicode_class(escape)?
                            .expect("Unicode class prefix was checked");
                        begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                        output.push_str(&class);
                        cursor += consumed;
                        record_re2_atom(&mut groups, 1);
                    }
                    _ => {
                        let (rune, consumed) = parse_re2_escape(escape)?;
                        begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                        // RE2 accepts surrogate escape atoms, but they cannot
                        // match a valid UTF-8 input. Use the same empty scalar
                        // set as surrogate-only bracket expressions.
                        push_class_rune(&mut output, rune);
                        cursor += consumed;
                        record_re2_atom(&mut groups, 1);
                    }
                }
            }
            '[' => {
                let (class, consumed) = translate_re2_class(&pattern[cursor..])?;
                begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                output.push_str(&class);
                cursor += consumed;
                record_re2_atom(&mut groups, 1);
            }
            '(' if pattern[cursor..].starts_with("(?") => {
                let perl = parse_re2_perl_group(&pattern[cursor..])?;
                cursor += perl.consumed;
                if perl.opens_group {
                    let parent = groups.last_mut().expect("root group");
                    begin_re2_atom(&mut output, parent);
                    let source_non_greedy =
                        perl.flags.non_greedy.unwrap_or(parent.source_non_greedy);
                    let output_start = output.len();
                    output.push_str(&perl.output);
                    groups.push(Re2GroupState {
                        source_non_greedy,
                        rust_non_greedy: source_non_greedy,
                        output_start,
                        ..Re2GroupState::default()
                    });
                } else {
                    let group = groups.last_mut().expect("root group");
                    group.pending_flags.push_str(&perl.output);
                    if let Some(non_greedy) = perl.flags.non_greedy {
                        group.source_non_greedy = non_greedy;
                    }
                    group.last_was_repeat = false;
                }
            }
            '(' => {
                let parent = groups.last_mut().expect("root group");
                begin_re2_atom(&mut output, parent);
                let source_non_greedy = parent.source_non_greedy;
                let output_start = output.len();
                output.push('(');
                cursor += 1;
                groups.push(Re2GroupState {
                    source_non_greedy,
                    rust_non_greedy: source_non_greedy,
                    output_start,
                    ..Re2GroupState::default()
                });
            }
            ')' => {
                if groups.len() == 1 {
                    return Err("unexpected closing parenthesis in RE2 pattern".to_string());
                }
                flush_re2_flags(&mut output, groups.last_mut().expect("nested group"));
                output.push(')');
                cursor += 1;
                let group = groups.pop().expect("non-root group");
                groups.last_mut().expect("root group").last_atom_start = group.output_start;
                record_re2_atom(&mut groups, group.max_repeat.max(1));
            }
            '|' => {
                flush_re2_flags(&mut output, groups.last_mut().expect("root group"));
                output.push('|');
                cursor += 1;
                let group = groups.last_mut().expect("root group");
                group.last_atom = None;
                group.last_was_repeat = false;
            }
            '*' | '+' | '?' => {
                let group = groups.last_mut().expect("root group");
                if group.last_atom.is_none() {
                    return Err(format!("RE2 repetition {character} has no argument"));
                }
                if group.last_was_repeat {
                    return Err(format!("invalid repeated RE2 operator {character}"));
                }
                group.wrap_quantified_atom(&mut output);
                output.push(character);
                cursor += 1;
                let source_inverted = pattern[cursor..].starts_with('?');
                if source_inverted {
                    cursor += 1;
                }
                let source_non_greedy = group.source_non_greedy ^ source_inverted;
                if source_non_greedy != group.rust_non_greedy {
                    output.push('?');
                }
                group.last_was_repeat = true;
                flush_re2_flags(&mut output, group);
            }
            '{' => {
                if let Some(repetition) = parse_re2_repetition(&pattern[cursor..]) {
                    let group = groups.last_mut().expect("root group");
                    let Some(atom_repeat) = group.last_atom else {
                        return Err("RE2 counted repetition has no argument".to_string());
                    };
                    if group.last_was_repeat {
                        return Err("invalid repeated RE2 counted repetition".to_string());
                    }
                    group.wrap_quantified_atom(&mut output);
                    if repetition.min > 1000 || repetition.max.is_some_and(|max| max > 1000) {
                        return Err("RE2 repetition count exceeds 1000".to_string());
                    }
                    if repetition.max.is_some_and(|max| max < repetition.min) {
                        return Err("RE2 repetition maximum is less than minimum".to_string());
                    }
                    let multiplier = repetition.max.unwrap_or(repetition.min).max(1);
                    let nested = atom_repeat.saturating_mul(multiplier);
                    if nested > 1000 {
                        return Err("nested RE2 repetition count exceeds 1000".to_string());
                    }
                    output.push_str(&pattern[cursor..cursor + repetition.consumed]);
                    cursor += repetition.consumed;
                    let source_inverted = pattern[cursor..].starts_with('?');
                    if source_inverted {
                        cursor += 1;
                    }
                    let source_non_greedy = group.source_non_greedy ^ source_inverted;
                    if source_non_greedy != group.rust_non_greedy {
                        output.push('?');
                    }
                    group.last_atom = Some(nested);
                    group.max_repeat = group.max_repeat.max(nested);
                    group.last_was_repeat = true;
                    flush_re2_flags(&mut output, group);
                } else {
                    begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                    push_regex_rune(&mut output, character as u32);
                    cursor += 1;
                    record_re2_atom(&mut groups, 1);
                }
            }
            '.' | '^' | '$' => {
                begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                output.push(character);
                cursor += 1;
                record_re2_atom(&mut groups, 1);
            }
            _ => {
                begin_re2_atom(&mut output, groups.last_mut().expect("root group"));
                push_regex_rune(&mut output, character as u32);
                cursor += character.len_utf8();
                record_re2_atom(&mut groups, 1);
            }
        }
    }
    if groups.len() != 1 {
        return Err("unclosed parenthesis in RE2 pattern".to_string());
    }
    flush_re2_flags(&mut output, groups.last_mut().expect("root group"));
    Ok(output)
}

#[derive(Default)]
struct Re2GroupState {
    max_repeat: u32,
    last_atom: Option<u32>,
    last_was_repeat: bool,
    source_non_greedy: bool,
    rust_non_greedy: bool,
    pending_flags: String,
    output_start: usize,
    last_atom_start: usize,
    atom_quantified: bool,
}

struct Re2PerlGroup {
    output: String,
    consumed: usize,
    opens_group: bool,
    flags: Re2FlagChanges,
}

#[derive(Clone, Copy, Default)]
struct Re2FlagChanges {
    insensitive: Option<bool>,
    multi_line: Option<bool>,
    dot_matches_new_line: Option<bool>,
    non_greedy: Option<bool>,
}

struct Re2Repetition {
    min: u32,
    max: Option<u32>,
    consumed: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn record_re2_atom(groups: &mut [Re2GroupState], repeat: u32) {
    let group = groups.last_mut().expect("root group");
    group.max_repeat = group.max_repeat.max(repeat);
    group.last_atom = Some(repeat);
    group.last_was_repeat = false;
    group.atom_quantified = false;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Re2GroupState {
    fn wrap_quantified_atom(&mut self, output: &mut String) {
        if self.atom_quantified && !self.last_was_repeat {
            output.insert_str(self.last_atom_start, "(?:");
            output.push(')');
        }
        self.atom_quantified = true;
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn flush_re2_flags(output: &mut String, group: &mut Re2GroupState) {
    output.push_str(&group.pending_flags);
    group.pending_flags.clear();
    group.rust_non_greedy = group.source_non_greedy;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn begin_re2_atom(output: &mut String, group: &mut Re2GroupState) {
    flush_re2_flags(output, group);
    group.last_atom_start = output.len();
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_perl_group(input: &str) -> std::result::Result<Re2PerlGroup, String> {
    debug_assert!(input.starts_with("(?"));
    if let Some(named) = input.strip_prefix("(?P<") {
        let Some(end) = named.find('>') else {
            return Err("unclosed RE2 named capture".to_string());
        };
        let name = &named[..end];
        if !is_valid_re2_capture_name(name) {
            return Err(format!("invalid RE2 capture name {name:?}"));
        }
        return Ok(Re2PerlGroup {
            // Capture names do not affect boolean FullMatch. Erasing the name
            // preserves RE2 names that Rust rejects, such as digit-leading
            // names, without admitting Rust's extra named-group spellings.
            output: "(?:".to_string(),
            consumed: 4 + end + 1,
            opens_group: true,
            flags: Re2FlagChanges::default(),
        });
    }

    let mut cursor = 2;
    let mut negated = false;
    let mut saw_flags = false;
    let mut flags = Re2FlagChanges::default();
    loop {
        let Some(flag) = input[cursor..].chars().next() else {
            return Err("incomplete RE2 Perl group".to_string());
        };
        cursor += flag.len_utf8();
        match flag {
            'i' | 'm' | 's' | 'U' => {
                saw_flags = true;
                let setting = Some(!negated);
                match flag {
                    'i' => flags.insensitive = setting,
                    'm' => flags.multi_line = setting,
                    's' => flags.dot_matches_new_line = setting,
                    'U' => flags.non_greedy = setting,
                    _ => unreachable!(),
                }
            }
            '-' if !negated => {
                negated = true;
                saw_flags = false;
            }
            ':' | ')' if !negated || saw_flags => {
                let opens_group = flag == ':';
                return Ok(Re2PerlGroup {
                    output: render_re2_flags(flags, opens_group),
                    consumed: cursor,
                    opens_group,
                    flags,
                });
            }
            _ => {
                return Err(format!(
                    "invalid RE2 Perl group near {:?}",
                    &input[..cursor]
                ));
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn render_re2_flags(flags: Re2FlagChanges, opens_group: bool) -> String {
    let mut enabled = String::new();
    let mut disabled = String::new();
    for (flag, setting) in [
        ('i', flags.insensitive),
        ('m', flags.multi_line),
        ('s', flags.dot_matches_new_line),
        ('U', flags.non_greedy),
    ] {
        match setting {
            Some(true) => enabled.push(flag),
            Some(false) => disabled.push(flag),
            None => {}
        }
    }
    if enabled.is_empty() && disabled.is_empty() {
        return if opens_group { "(?:" } else { "" }.to_string();
    }
    let mut output = String::from("(?");
    output.push_str(&enabled);
    if !disabled.is_empty() {
        output.push('-');
        output.push_str(&disabled);
    }
    output.push(if opens_group { ':' } else { ')' });
    output
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn is_valid_re2_capture_name(name: &str) -> bool {
    static CAPTURE_NAME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

    !name.is_empty()
        && CAPTURE_NAME
            .get_or_init(|| {
                regex::Regex::new(
                    r"\A[\p{Lu}\p{Ll}\p{Lt}\p{Lm}\p{Lo}\p{Nl}\p{Mn}\p{Mc}\p{Nd}\p{Pc}]+\z",
                )
                .expect("static RE2 capture-name class")
            })
            .is_match(name)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_repetition(input: &str) -> Option<Re2Repetition> {
    let rest = input.strip_prefix('{')?;
    let (min, mut cursor) = parse_re2_decimal(rest)?;
    let mut max = Some(min);
    if rest[cursor..].starts_with(',') {
        cursor += 1;
        if rest[cursor..].starts_with('}') {
            max = None;
        } else {
            let (parsed, consumed) = parse_re2_decimal(&rest[cursor..])?;
            max = Some(parsed);
            cursor += consumed;
        }
    }
    if !rest[cursor..].starts_with('}') {
        return None;
    }
    Some(Re2Repetition {
        min,
        max,
        consumed: 1 + cursor + 1,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_decimal(input: &str) -> Option<(u32, usize)> {
    let bytes = input.as_bytes();
    let first = *bytes.first()?;
    if !first.is_ascii_digit() || (first == b'0' && bytes.get(1).is_some_and(u8::is_ascii_digit)) {
        return None;
    }
    let mut value = 0_u32;
    let mut cursor = 0;
    while let Some(byte) = bytes.get(cursor).copied().filter(u8::is_ascii_digit) {
        if value >= 100_000_000 {
            return None;
        }
        value = value * 10 + u32::from(byte - b'0');
        cursor += 1;
    }
    Some((value, cursor))
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

        if let Some((class, consumed)) = parse_re2_posix_class(&input[cursor..])? {
            output.push_str(class);
            cursor += consumed;
            continue;
        }
        if let Some((class, consumed)) = parse_re2_unicode_class(&input[cursor..])? {
            output.push_str(&class);
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
                    low, high
                ));
            }
            push_class_range(&mut output, low, high);
        } else {
            push_class_rune(&mut output, low);
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_posix_class(input: &str) -> std::result::Result<Option<(&str, usize)>, String> {
    let Some(rest) = input.strip_prefix("[:") else {
        return Ok(None);
    };
    let Some(end) = rest.find(":]") else {
        // ParseCCName returns kParseNothing, so the opening `[` is an
        // ordinary class rune when no POSIX terminator exists.
        return Ok(None);
    };
    let name = &rest[..end];
    let base = name.strip_prefix('^').unwrap_or(name);
    const POSIX_NAMES: &[&str] = &[
        "alnum", "alpha", "ascii", "blank", "cntrl", "digit", "graph", "lower", "print", "punct",
        "space", "upper", "word", "xdigit",
    ];
    if name.is_empty() || !POSIX_NAMES.contains(&base) {
        return Err(format!("invalid RE2 POSIX character class {name:?}"));
    }
    let consumed = 2 + end + 2;
    Ok(Some((&input[..consumed], consumed)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_unicode_class(input: &str) -> std::result::Result<Option<(String, usize)>, String> {
    let Some(kind) = input.as_bytes().get(1).copied() else {
        return Ok(None);
    };
    if !input.starts_with('\\') || !matches!(kind, b'p' | b'P') {
        return Ok(None);
    }
    let rest = &input[2..];
    let Some(first) = rest.chars().next() else {
        return Err("incomplete RE2 Unicode character class".to_string());
    };
    let (mut name, consumed) = if first == '{' {
        let Some(end) = rest.find('}') else {
            return Err("unclosed RE2 Unicode character class".to_string());
        };
        (&rest[1..end], 2 + end + 1)
    } else {
        (&rest[..first.len_utf8()], 2 + first.len_utf8())
    };
    let mut negated = kind == b'P';
    if let Some(without_caret) = name.strip_prefix('^') {
        name = without_caret;
        negated = !negated;
    }
    // RE2 handles `Any` directly in ParseUnicodeGroup rather than placing it
    // in the generated unicode_groups.cc table.
    if name.is_empty()
        || (name != "Any"
            && !RE2_UNICODE_GROUP_NAMES
                .split('|')
                .any(|group| group == name))
    {
        return Err(format!("invalid RE2 Unicode character class {name:?}"));
    }
    Ok(Some((
        format!("\\{}{{{name}}}", if negated { 'P' } else { 'p' }),
        consumed,
    )))
}

// Exact `UGroup` names from both pinned `re2/unicode_groups.cc` files.
const RE2_UNICODE_GROUP_NAMES: &str = "Adlam|Ahom|Anatolian_Hieroglyphs|Arabic|Armenian|Avestan|Balinese|Bamum|Bassa_Vah|Batak|Bengali|Bhaiksuki|Bopomofo|Brahmi|Braille|Buginese|Buhid|C|Canadian_Aboriginal|Carian|Caucasian_Albanian|Cc|Cf|Chakma|Cham|Cherokee|Chorasmian|Co|Common|Coptic|Cs|Cuneiform|Cypriot|Cypro_Minoan|Cyrillic|Deseret|Devanagari|Dives_Akuru|Dogra|Duployan|Egyptian_Hieroglyphs|Elbasan|Elymaic|Ethiopic|Georgian|Glagolitic|Gothic|Grantha|Greek|Gujarati|Gunjala_Gondi|Gurmukhi|Han|Hangul|Hanifi_Rohingya|Hanunoo|Hatran|Hebrew|Hiragana|Imperial_Aramaic|Inherited|Inscriptional_Pahlavi|Inscriptional_Parthian|Javanese|Kaithi|Kannada|Katakana|Kawi|Kayah_Li|Kharoshthi|Khitan_Small_Script|Khmer|Khojki|Khudawadi|L|Lao|Latin|Lepcha|Limbu|Linear_A|Linear_B|Lisu|Ll|Lm|Lo|Lt|Lu|Lycian|Lydian|M|Mahajani|Makasar|Malayalam|Mandaic|Manichaean|Marchen|Masaram_Gondi|Mc|Me|Medefaidrin|Meetei_Mayek|Mende_Kikakui|Meroitic_Cursive|Meroitic_Hieroglyphs|Miao|Mn|Modi|Mongolian|Mro|Multani|Myanmar|N|Nabataean|Nag_Mundari|Nandinagari|Nd|New_Tai_Lue|Newa|Nko|Nl|No|Nushu|Nyiakeng_Puachue_Hmong|Ogham|Ol_Chiki|Old_Hungarian|Old_Italic|Old_North_Arabian|Old_Permic|Old_Persian|Old_Sogdian|Old_South_Arabian|Old_Turkic|Old_Uyghur|Oriya|Osage|Osmanya|P|Pahawh_Hmong|Palmyrene|Pau_Cin_Hau|Pc|Pd|Pe|Pf|Phags_Pa|Phoenician|Pi|Po|Ps|Psalter_Pahlavi|Rejang|Runic|S|Samaritan|Saurashtra|Sc|Sharada|Shavian|Siddham|SignWriting|Sinhala|Sk|Sm|So|Sogdian|Sora_Sompeng|Soyombo|Sundanese|Syloti_Nagri|Syriac|Tagalog|Tagbanwa|Tai_Le|Tai_Tham|Tai_Viet|Takri|Tamil|Tangsa|Tangut|Telugu|Thaana|Thai|Tibetan|Tifinagh|Tirhuta|Toto|Ugaritic|Vai|Vithkuqi|Wancho|Warang_Citi|Yezidi|Yi|Z|Zanabazar_Square|Zl|Zp|Zs";

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
fn parse_re2_class_character(input: &str) -> std::result::Result<(u32, usize), String> {
    let Some(character) = input.chars().next() else {
        return Err("missing RE2 character class rune".to_string());
    };
    if character != '\\' {
        return Ok((character as u32, character.len_utf8()));
    }
    let Some(escaped) = input[1..].chars().next() else {
        return Err("trailing backslash in RE2 character class".to_string());
    };
    let escaped_len = escaped.len_utf8();
    if escaped.is_ascii() && !escaped.is_ascii_alphanumeric() {
        return Ok((escaped as u32, 1 + escaped_len));
    }
    match escaped {
        '0'..='7' => parse_re2_octal_escape(input, escaped),
        'x' => parse_re2_hex_escape(input),
        'n' => Ok(('\n' as u32, 2)),
        'r' => Ok(('\r' as u32, 2)),
        't' => Ok(('\t' as u32, 2)),
        'a' => Ok((0x07, 2)),
        'f' => Ok((0x0C, 2)),
        'v' => Ok((0x0B, 2)),
        _ => Err(format!("invalid RE2 character class escape \\{escaped}")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_escape(input: &str) -> std::result::Result<(u32, usize), String> {
    parse_re2_class_character(input).map_err(|error| error.replace(" character class", ""))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_octal_escape(input: &str, first: char) -> std::result::Result<(u32, usize), String> {
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
    Ok((value, consumed))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_re2_hex_escape(input: &str) -> std::result::Result<(u32, usize), String> {
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
    if value > 0x10_FFFF {
        return Err(format!("invalid RE2 hexadecimal escape value {value:X}"));
    }
    Ok((value, consumed))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn push_class_rune(output: &mut String, rune: u32) {
    if (0xD800..=0xDFFF).contains(&rune) {
        push_empty_class(output);
    } else {
        push_regex_rune(output, rune);
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn push_class_range(output: &mut String, low: u32, high: u32) {
    if high < 0xD800 || low > 0xDFFF {
        push_regex_rune(output, low);
        output.push('-');
        push_regex_rune(output, high);
    } else if low >= 0xD800 && high <= 0xDFFF {
        push_empty_class(output);
    } else {
        if low < 0xD800 {
            push_regex_rune(output, low);
            output.push('-');
            push_regex_rune(output, 0xD7FF);
        }
        if high > 0xDFFF {
            push_regex_rune(output, 0xE000);
            output.push('-');
            push_regex_rune(output, high);
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn push_empty_class(output: &mut String) {
    // Rust supports nested class intersection. This expression is the empty
    // scalar set, which represents RE2 surrogate runes that cannot occur in a
    // valid UTF-8 input without turning the overall class into a parse error.
    output.push_str(r"[\x{0}&&[^\x{0}]]");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn push_regex_rune(output: &mut String, rune: u32) {
    use std::fmt::Write;

    write!(output, "\\x{{{rune:X}}}").expect("writing to a String cannot fail");
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

struct ParsedStatement {
    token: Token,
    args: Vec<String>,
    sql: ByteSection,
    error: ByteSection,
    declaration: DeclarationId,
    conditions: Vec<Condition>,
}

struct ParsedQuery {
    token: Token,
    args: Vec<String>,
    sql: ByteSection,
    expected: Vec<SourceLine>,
    declaration: DeclarationId,
    conditions: Vec<Condition>,
}

enum LoopCommand {
    Statement(ParsedStatement),
    Query(ParsedQuery),
    Loop(ParsedLoop),
    Continue {
        token: Token,
        conditions: Vec<Condition>,
    },
    Load(Token),
    Restart(Token),
    Reconnect(Token),
    Reset(Token),
}

struct ParsedLoop {
    token: Token,
    definition: LoopDefinition,
    body: Vec<LoopCommand>,
}

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
/// and typed result oracle, including loop scheduling, restart/load, and
/// concurrent controls.
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
    let matcher = RustRe2Matcher;
    let oracle = Mutex::new(
        Oracle::new()
            .with_regex(&matcher)
            .with_substitutions(&substitutions)
            .with_files(&expected_files),
    );
    let mut parser = SqlLogicParser::from_bytes(&path, &source);
    let accounting = Mutex::new(RecordAccounting::default());
    let mut sessions = Sessions::new(database, &scratch.0);
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
            if system
                .bytes()
                .any(|byte| matches!(byte, b'=' | b'<' | b'>'))
                && !original_sqlite
            {
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
                let mut accounting = accounting.lock().expect("record accounting poisoned");
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
                    &mut sessions,
                    &mut parser,
                    &accounting,
                    &oracle,
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
                    &mut sessions,
                    &mut parser,
                    &accounting,
                    &oracle,
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
                    .lock()
                    .expect("oracle poisoned")
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
                        let snapshot = accounting
                            .lock()
                            .expect("record accounting poisoned")
                            .snapshot();
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
            TokenKind::Load => {
                let args = string_arguments(&token)?;
                if args.len() > 3 {
                    return Err(at(
                        &token,
                        "load accepts PATH [readonly|readwrite] [VERSION]",
                    ));
                }
                let read_only = match args.get(1).map(String::as_str) {
                    None | Some("readwrite") => false,
                    Some("readonly") => true,
                    Some(value) => return Err(at(&token, &format!("invalid load mode {value:?}"))),
                };
                if let Some(version) = args.get(2) {
                    return Err(at(
                        &token,
                        &format!("storage compatibility version {version:?} is not available"),
                    ));
                }
                let path = args
                    .first()
                    .map(|path| substitutions.replace(path.as_bytes()))
                    .transpose_utf8(&token, "load path")?
                    .map(PathBuf::from);
                sessions
                    .load(path, read_only)
                    .map_err(|error| at(&token, &error.to_string()))?;
            }
            TokenKind::Restart => {
                let args = string_arguments(&token)?;
                if !(args.is_empty() || args == ["no_extension_load"]) {
                    return Err(at(
                        &token,
                        "restart accepts only optional no_extension_load",
                    ));
                }
                sessions
                    .restart()
                    .map_err(|error| at(&token, &error.to_string()))?;
            }
            TokenKind::Reconnect => {
                if !token.parameters.is_empty() {
                    return Err(at(&token, "reconnect accepts no arguments"));
                }
                sessions.reconnect();
            }
            TokenKind::Loop
            | TokenKind::Foreach
            | TokenKind::ConcurrentLoop
            | TokenKind::ConcurrentForeach => {
                let command = capture_loop(
                    &mut parser,
                    &mut fixtures,
                    &mut active_sources,
                    &accounting,
                    &mut sessions,
                    &token,
                    allow_missing_error,
                    original_sqlite,
                )?;
                generated += execute_loop(
                    &command,
                    &mut sessions,
                    &accounting,
                    &oracle,
                    &substitutions,
                    hash_threshold,
                    original_sqlite,
                    directives.mode,
                    &mut output,
                    &[],
                    None,
                )?;
            }
            TokenKind::EndLoop => return Err(at(&token, "endloop without active loop")),
            TokenKind::Invalid => return Err(at(&token, "invalid SQLLogicTest directive")),
        }
    }
    fixtures.finish_script();
    let snapshot = accounting
        .lock()
        .expect("record accounting poisoned")
        .snapshot();
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
fn loop_conditions(
    parser: &mut SqlLogicParser,
    mut token: Token,
    original_sqlite: bool,
) -> Result<(Vec<Condition>, Token)> {
    let mut conditions = Vec::new();
    while matches!(token.kind, TokenKind::SkipIf | TokenKind::OnlyIf) {
        let text = one_argument(&token, "skipif/onlyif")?;
        let text = ascii(text, &token, "condition")?.to_ascii_lowercase();
        for term in text.split("&&") {
            conditions.push(
                schedule::parse_condition(term, token.kind == TokenKind::SkipIf, original_sqlite)
                    .map_err(|error| at(&token, &error.to_string()))?,
            );
        }
        parser.next_line();
        token = parser.tokenize().map_err(parse_error)?;
    }
    Ok((conditions, token))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn capture_loop(
    parser: &mut SqlLogicParser,
    fixtures: &mut FixtureResolver,
    active_sources: &mut Vec<PathBuf>,
    accounting: &Mutex<RecordAccounting>,
    sessions: &mut Sessions,
    header: &Token,
    allow_missing_error: bool,
    original_sqlite: bool,
) -> Result<ParsedLoop> {
    let arguments = string_arguments(header)?;
    let foreach = matches!(
        header.kind,
        TokenKind::Foreach | TokenKind::ConcurrentForeach
    );
    let concurrent = matches!(
        header.kind,
        TokenKind::ConcurrentLoop | TokenKind::ConcurrentForeach
    );
    let definition = schedule::parse_loop_with(&arguments, concurrent, foreach, |specification| {
        let (connection, name) = specification
            .split_once(':')
            .map_or(("", specification), |(connection, name)| (connection, name));
        let name = name.replace('\'', "''");
        let result = sessions
            .connection(connection)?
            .query(&format!("SELECT unnest(getvariable('{name}')::VARCHAR[])"))?;
        Ok(result
            .rows
            .iter()
            .map(|row| {
                row.first()
                    .map(Value::to_string)
                    .unwrap_or_else(|| "NULL".into())
            })
            .collect())
    })
    .map_err(|error| at(header, &error.to_string()))?;
    let mut body = Vec::new();
    loop {
        let Some(start) = parser.next_statement().map_err(parse_error)? else {
            return Err(at(header, "missing endloop"));
        };
        sync_include_stack(fixtures, active_sources, &start.location)?;
        let token = parser.tokenize().map_err(parse_error)?;
        if token.kind.is_single_line() && !parser.next_line_empty_or_comment() {
            return Err(at(
                &token,
                "all test statements need to be separated by an empty line",
            ));
        }
        let (conditions, token) = loop_conditions(parser, token, original_sqlite)?;
        match token.kind {
            TokenKind::Statement => body.push(LoopCommand::Statement(parse_statement(
                parser,
                accounting,
                &token,
                allow_missing_error,
                conditions,
            )?)),
            TokenKind::Query => body.push(LoopCommand::Query(parse_query(
                parser, accounting, &token, conditions,
            )?)),
            TokenKind::Loop
            | TokenKind::Foreach
            | TokenKind::ConcurrentLoop
            | TokenKind::ConcurrentForeach => {
                if !conditions.is_empty() {
                    return Err(at(&token, "conditions on loop controls are not supported"));
                }
                body.push(LoopCommand::Loop(capture_loop(
                    parser,
                    fixtures,
                    active_sources,
                    accounting,
                    sessions,
                    &token,
                    allow_missing_error,
                    original_sqlite,
                )?));
            }
            TokenKind::Continue => body.push(LoopCommand::Continue { token, conditions }),
            TokenKind::Load => body.push(LoopCommand::Load(token)),
            TokenKind::Restart => body.push(LoopCommand::Restart(token)),
            TokenKind::Reconnect => body.push(LoopCommand::Reconnect(token)),
            TokenKind::Reset => body.push(LoopCommand::Reset(token)),
            TokenKind::EndLoop => {
                if !conditions.is_empty() {
                    return Err(at(&token, "conditions cannot precede endloop"));
                }
                return Ok(ParsedLoop {
                    token: header.clone(),
                    definition,
                    body,
                });
            }
            TokenKind::Include => {
                if !conditions.is_empty() {
                    return Err(at(&token, "conditions cannot precede include"));
                }
                let include = one_argument(&token, "include")?;
                let include = ascii(include, &token, "include path")?;
                let current = active_sources
                    .last()
                    .ok_or_else(|| Error::Internal("empty include stack".into()))?;
                let location = directive_header(&token)?.location;
                let include_path = fixtures
                    .enter_include(current, include, location)
                    .map_err(fixture_error)?;
                let bytes = std::fs::read(&include_path)?;
                parser.push_include(&include_path, bytes);
                active_sources.push(include_path);
            }
            _ => {
                return Err(at(
                    &token,
                    "control directive is not supported inside a loop by the integrated runner",
                ));
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn concurrent_supported(command: &LoopCommand) -> Result<()> {
    match command {
        LoopCommand::Statement(statement) => {
            if statement.args.get(1).is_some_and(|name| !name.is_empty()) {
                return Err(at(
                    &statement.token,
                    "Named connections not supported in parallel loop",
                ));
            }
        }
        LoopCommand::Query(query) => {
            if query
                .args
                .get(1)
                .is_some_and(|argument| sort_mode(argument).is_none())
            {
                return Err(at(
                    &query.token,
                    "Named connections not supported in parallel loop",
                ));
            }
        }
        LoopCommand::Loop(loop_command) if loop_command.definition.concurrent => {
            return Err(at(
                &loop_command.token,
                "Nested parallel loop commands not allowed",
            ));
        }
        LoopCommand::Loop(loop_command) => {
            for nested in &loop_command.body {
                concurrent_supported(nested)?;
            }
        }
        LoopCommand::Continue { token, .. }
        | LoopCommand::Load(token)
        | LoopCommand::Restart(token)
        | LoopCommand::Reconnect(token) => {
            return Err(at(
                token,
                "Concurrent loop is not supported over this command",
            ));
        }
        LoopCommand::Reset(_) => {}
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_loop(
    command: &ParsedLoop,
    sessions: &mut Sessions,
    accounting: &Mutex<RecordAccounting>,
    oracle: &Mutex<Oracle<'_>>,
    substitutions: &Substitutions,
    hash_threshold: usize,
    original_sqlite: bool,
    mode: Mode,
    output: &mut Vec<String>,
    outer_loops: &[LoopFrame],
    finished: Option<&AtomicBool>,
) -> Result<usize> {
    if command.definition.concurrent {
        for body in &command.body {
            concurrent_supported(body)?;
        }
        let database = sessions.database().clone();
        let scratch = sessions.scratch().to_path_buf();
        let concurrent_finished = AtomicBool::new(false);
        let outcomes = schedule::run_concurrent(command.definition.values.len(), |ordinal| {
            if concurrent_finished.load(Ordering::Acquire) {
                return Ok((0, Vec::new()));
            }
            let mut local_sessions = Sessions::new(&database, &scratch);
            let mut loops = outer_loops.to_vec();
            loops.push(LoopFrame {
                name: command.definition.name.clone(),
                value: command.definition.values[ordinal].clone(),
                ordinal,
                concurrent: true,
            });
            let mut local_output = Vec::new();
            let mut generated = 0;
            let result = execute_loop_body(
                &command.body,
                &mut local_sessions,
                accounting,
                oracle,
                substitutions,
                hash_threshold,
                original_sqlite,
                mode,
                &mut local_output,
                &loops,
                &mut generated,
                Some(&concurrent_finished),
            );
            if result.is_err() {
                concurrent_finished.store(true, Ordering::Release);
            }
            result?;
            Ok((generated, local_output))
        })?;
        let mut generated = 0;
        let mut failure = None;
        for outcome in outcomes {
            match outcome {
                Ok((count, mut local_output)) => {
                    generated += count;
                    output.append(&mut local_output);
                }
                Err(error) if failure.is_none() => failure = Some(error),
                Err(_) => {}
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(generated)
    } else {
        let mut generated = 0;
        for (ordinal, value) in command.definition.values.iter().enumerate() {
            let mut loops = outer_loops.to_vec();
            loops.push(LoopFrame {
                name: command.definition.name.clone(),
                value: value.clone(),
                ordinal,
                concurrent: false,
            });
            if execute_loop_body(
                &command.body,
                sessions,
                accounting,
                oracle,
                substitutions,
                hash_threshold,
                original_sqlite,
                mode,
                output,
                &loops,
                &mut generated,
                finished,
            )? {
                continue;
            }
        }
        Ok(generated)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_loop_body(
    body: &[LoopCommand],
    sessions: &mut Sessions,
    accounting: &Mutex<RecordAccounting>,
    oracle: &Mutex<Oracle<'_>>,
    substitutions: &Substitutions,
    hash_threshold: usize,
    original_sqlite: bool,
    mode: Mode,
    output: &mut Vec<String>,
    loops: &[LoopFrame],
    generated: &mut usize,
    finished: Option<&AtomicBool>,
) -> Result<bool> {
    for command in body {
        if finished.is_some_and(|finished| finished.load(Ordering::Acquire)) {
            break;
        }
        match command {
            LoopCommand::Statement(statement) => {
                *generated += usize::from(matches!(
                    statement.args.first().map(String::as_str),
                    Some("debug" | "debug_skip")
                ));
                let _ = execute_parsed_statement(
                    sessions,
                    accounting,
                    oracle,
                    substitutions,
                    statement,
                    loops,
                    original_sqlite,
                    mode,
                    output,
                )?;
            }
            LoopCommand::Query(query) => {
                execute_parsed_query(
                    sessions,
                    accounting,
                    oracle,
                    substitutions,
                    query,
                    loops,
                    hash_threshold,
                    original_sqlite,
                    mode,
                    output,
                )?;
                *generated += usize::from(mode.output_hash);
            }
            LoopCommand::Loop(loop_command) => {
                *generated += execute_loop(
                    loop_command,
                    sessions,
                    accounting,
                    oracle,
                    substitutions,
                    hash_threshold,
                    original_sqlite,
                    mode,
                    output,
                    loops,
                    finished,
                )?;
            }
            LoopCommand::Continue { token, conditions } => {
                if schedule::selected(conditions, loops, original_sqlite)
                    .map_err(|error| at(token, &error.to_string()))?
                {
                    return Ok(true);
                }
            }
            LoopCommand::Load(token) => execute_loop_load(sessions, substitutions, token, loops)?,
            LoopCommand::Restart(token) => {
                let args = string_arguments(token)?;
                if !(args.is_empty() || args == ["no_extension_load"]) {
                    return Err(at(token, "restart accepts only optional no_extension_load"));
                }
                sessions
                    .restart()
                    .map_err(|error| at(token, &error.to_string()))?;
            }
            LoopCommand::Reconnect(token) => {
                if !token.parameters.is_empty() {
                    return Err(at(token, "reconnect accepts no arguments"));
                }
                sessions.reconnect();
            }
            LoopCommand::Reset(token) => {
                let args = string_arguments(token)?;
                if args.len() != 2 || !args[0].eq_ignore_ascii_case("label") {
                    return Err(at(token, "expected reset label NAME"));
                }
                oracle
                    .lock()
                    .expect("oracle poisoned")
                    .reset_label(&args[1])
                    .map_err(|error| at(token, &error.to_string()))?;
            }
        }
    }
    Ok(false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn execute_loop_load(
    sessions: &mut Sessions,
    substitutions: &Substitutions,
    token: &Token,
    loops: &[LoopFrame],
) -> Result<()> {
    let args = string_arguments(token)?;
    if args.len() > 2 {
        return Err(at(
            token,
            "load in a loop supports PATH [readonly|readwrite]",
        ));
    }
    let read_only = match args.get(1).map(String::as_str) {
        None | Some("readwrite") => false,
        Some("readonly") => true,
        Some(value) => return Err(at(token, &format!("invalid load mode {value:?}"))),
    };
    let path = args
        .first()
        .map(|path| schedule::replace_loops(substitutions.replace(path.as_bytes()), loops))
        .transpose()?
        .transpose_utf8(token, "load path")?
        .map(PathBuf::from);
    sessions
        .load(path, read_only)
        .map_err(|error| at(token, &error.to_string()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_statement(
    sessions: &mut Sessions,
    parser: &mut SqlLogicParser,
    accounting: &Mutex<RecordAccounting>,
    oracle: &Mutex<Oracle<'_>>,
    substitutions: &Substitutions,
    token: &Token,
    allow_missing_error: bool,
    mode: Mode,
    output: &mut Vec<String>,
) -> Result<bool> {
    let command = parse_statement(parser, accounting, token, allow_missing_error, Vec::new())?;
    execute_parsed_statement(
        sessions,
        accounting,
        oracle,
        substitutions,
        &command,
        &[],
        false,
        mode,
        output,
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_statement(
    parser: &mut SqlLogicParser,
    accounting: &Mutex<RecordAccounting>,
    token: &Token,
    allow_missing_error: bool,
    conditions: Vec<Condition>,
) -> Result<ParsedStatement> {
    let args = string_arguments(token)?;
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
    let declaration = accounting
        .lock()
        .expect("record accounting poisoned")
        .declare(token.location.clone());
    Ok(ParsedStatement {
        token: token.clone(),
        args,
        sql,
        error: error_section,
        declaration,
        conditions,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_parsed_statement(
    sessions: &mut Sessions,
    accounting: &Mutex<RecordAccounting>,
    oracle: &Mutex<Oracle<'_>>,
    substitutions: &Substitutions,
    command: &ParsedStatement,
    loops: &[LoopFrame],
    original_sqlite: bool,
    mode: Mode,
    output: &mut Vec<String>,
) -> Result<bool> {
    let token = &command.token;
    let args = &command.args;
    let statement_debug = matches!(
        args.first().map(String::as_str),
        Some("debug" | "debug_skip")
    );
    let debug_skip = args.first().is_some_and(|arg| arg == "debug_skip");
    let expected_error = substitutions.replace(&command.error.bytes);
    let expected = match args.first().map(String::as_str) {
        Some("ok") => ExpectedStatement::Success,
        Some("error") => ExpectedStatement::Error(
            (!expected_error.is_empty()).then_some(expected_error.as_slice()),
        ),
        Some("maybe") => ExpectedStatement::Unknown(
            (!expected_error.is_empty()).then_some(expected_error.as_slice()),
        ),
        Some("debug" | "debug_skip") => ExpectedStatement::DontCare,
        _ => unreachable!("validated while parsing"),
    };
    let connection_name = args.get(1).cloned().unwrap_or_default();
    let loop_iterations = loops.iter().map(|frame| frame.ordinal).collect();
    let execution = accounting
        .lock()
        .expect("record accounting poisoned")
        .plan(command.declaration, loop_iterations)
        .map_err(accounting_error)?;
    if !schedule::selected(&command.conditions, loops, original_sqlite)? {
        accounting
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Skipped)
            .map_err(accounting_error)?;
        return Ok(false);
    }
    let sql_bytes = schedule::replace_loops(substitutions.replace(&command.sql.bytes), loops)?;
    if mode.output_result || mode.debug || statement_debug {
        append_output_preamble(output, token, &sql_bytes);
    }
    let text = transport_sql(&sql_bytes).map_err(|error| {
        let _ = accounting
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Failed);
        at(token, &format!("{error}; SQL {}", escaped(&sql_bytes)))
    })?;
    let outcome = sessions.connection(&connection_name)?.execute(text);
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
        Ok(_) => oracle
            .lock()
            .expect("oracle poisoned")
            .check_statement(StatementResult::Success, expected),
        Err(error) => {
            let message = error.to_string();
            oracle.lock().expect("oracle poisoned").check_statement(
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
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Failed)
            .map_err(accounting_error)?;
        return Err(at(token, &format!("{error}; SQL {}", escaped(&sql_bytes))));
    }
    accounting
        .lock()
        .expect("record accounting poisoned")
        .record(execution, ExecutionOutcome::Passed)
        .map_err(accounting_error)?;
    Ok(debug_skip)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_query(
    sessions: &mut Sessions,
    parser: &mut SqlLogicParser,
    accounting: &Mutex<RecordAccounting>,
    oracle: &Mutex<Oracle<'_>>,
    substitutions: &Substitutions,
    token: &Token,
    hash_threshold: usize,
    original_sqlite: bool,
    mode: Mode,
    output: &mut Vec<String>,
) -> Result<()> {
    let command = parse_query(parser, accounting, token, Vec::new())?;
    execute_parsed_query(
        sessions,
        accounting,
        oracle,
        substitutions,
        &command,
        &[],
        hash_threshold,
        original_sqlite,
        mode,
        output,
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_query(
    parser: &mut SqlLogicParser,
    accounting: &Mutex<RecordAccounting>,
    token: &Token,
    conditions: Vec<Condition>,
) -> Result<ParsedQuery> {
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
    parser.next_line();
    let sql = parser.extract_statement();
    let expected_lines = parser.extract_expected_result();
    let declaration = accounting
        .lock()
        .expect("record accounting poisoned")
        .declare(token.location.clone());
    Ok(ParsedQuery {
        token: token.clone(),
        args,
        sql,
        expected: expected_lines,
        declaration,
        conditions,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(clippy::too_many_arguments)]
fn execute_parsed_query(
    sessions: &mut Sessions,
    accounting: &Mutex<RecordAccounting>,
    oracle: &Mutex<Oracle<'_>>,
    substitutions: &Substitutions,
    command: &ParsedQuery,
    loops: &[LoopFrame],
    hash_threshold: usize,
    original_sqlite: bool,
    mode: Mode,
    output: &mut Vec<String>,
) -> Result<()> {
    let token = &command.token;
    let signature = &command.args[0];
    let mut sort = SortMode::None;
    let mut connection_name = String::new();
    if let Some(second) = command.args.get(1) {
        if let Some(parsed) = sort_mode(second) {
            sort = parsed;
        } else {
            connection_name = second.clone();
        }
    }
    let label = command.args.get(2).map(String::as_str);
    let mut expected_owned: Vec<_> = command
        .expected
        .iter()
        .map(|line| substitutions.replace(line.normalized()))
        .collect();
    // Pinned LoopReplacement applies to SQL and to an external-result file
    // name, not to literal expected rows or expected-error text.
    if expected_owned.len() == 1 && expected_owned[0].starts_with(b"<FILE>:") {
        expected_owned[0] = schedule::replace_loops(std::mem::take(&mut expected_owned[0]), loops)?;
    }
    let expected: Vec<_> = expected_owned.iter().map(Vec::as_slice).collect();
    let loop_iterations = loops.iter().map(|frame| frame.ordinal).collect();
    let execution = accounting
        .lock()
        .expect("record accounting poisoned")
        .plan(command.declaration, loop_iterations)
        .map_err(accounting_error)?;
    if !schedule::selected(&command.conditions, loops, original_sqlite)? {
        accounting
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Skipped)
            .map_err(accounting_error)?;
        return Ok(());
    }
    let sql_bytes = schedule::replace_loops(substitutions.replace(&command.sql.bytes), loops)?;
    if mode.output_result || mode.debug {
        append_output_preamble(output, token, &sql_bytes);
    }
    let text = transport_sql(&sql_bytes).map_err(|error| {
        let _ = accounting
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Failed);
        at(token, &format!("{error}; SQL {}", escaped(&sql_bytes)))
    })?;
    let result = sessions
        .connection(&connection_name)?
        .query(text)
        .map_err(|error| {
            let _ = accounting
                .lock()
                .expect("record accounting poisoned")
                .record(execution, ExecutionOutcome::Failed);
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
        let _ = accounting
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Failed);
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
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Passed)
            .map_err(accounting_error)?;
        return Ok(());
    }
    let checked = oracle.lock().expect("oracle poisoned").check_query(
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
            .lock()
            .expect("record accounting poisoned")
            .record(execution, ExecutionOutcome::Failed)
            .map_err(accounting_error)?;
        return Err(at(token, &format!("{error}; SQL {}", escaped(&sql_bytes))));
    }
    accounting
        .lock()
        .expect("record accounting poisoned")
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
    fn default_directive_state_does_not_admit_ambient_environment() {
        let state = default_directive_state();
        assert!(state.environment.is_empty());
        assert!(state.configured_environment.is_empty());
        assert!(state.passthrough_environment.is_empty());
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
    fn regex_byte_atoms_quotes_and_surrogate_classes_match_re2() {
        let matcher = RustRe2Matcher;

        assert!(!matcher.full_match(br"\C", "é".as_bytes()).unwrap());
        assert!(matcher.full_match(br"\C\C", "é".as_bytes()).unwrap());
        assert!(matcher.full_match(br"\C", b"a").unwrap());
        assert!(matcher.full_match(br"\C", b"\n").unwrap());

        for (pattern, value) in [
            (r"\Q.*[x](a)\E", ".*[x](a)"),
            (r"\Q[a&&b]\E", "[a&&b]"),
            (r"\Q\\E", "\\"),
            (r"\Qabc[", "abc["),
            (r"(?i)\QABC\E", "abc"),
        ] {
            assert!(
                matcher
                    .full_match(pattern.as_bytes(), value.as_bytes())
                    .unwrap(),
                "{pattern:?} must quote {value:?}"
            );
        }
        assert!(matcher.full_match(br"\E", b"").is_err());

        for pattern in [r"[\x{D800}]", r"[\x{D800}-\x{DFFF}]"] {
            assert!(
                !matcher.full_match(pattern.as_bytes(), b"a").unwrap(),
                "surrogate-only class {pattern:?} must be empty"
            );
        }
        assert!(!matcher.full_match(br"\x{D800}", b"a").unwrap());
        assert!(matcher.full_match(br"[^\x{D800}-\x{DFFF}]", b"a").unwrap());
        assert!(matcher.full_match(br"[a\x{D800}]", b"a").unwrap());
        assert!(
            matcher
                .full_match(br"[\x{D7FF}-\x{E000}]", "\u{D7FF}".as_bytes())
                .unwrap()
        );
        assert!(
            matcher
                .full_match(br"[\x{D7FF}-\x{E000}]", "\u{E000}".as_bytes())
                .unwrap()
        );
        assert!(matcher.full_match(br"[\x{110000}]", b"a").is_err());
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn regex_grammar_accepts_only_pinned_re2_constructs() {
        let matcher = RustRe2Matcher;
        for (pattern, value) in [
            (r"(?i:a)", "A"),
            (r"(?m:^a$)", "a"),
            (r"(?s:.)", "\n"),
            (r"(?U:a+)", "a"),
            (r"(?im-sU:a)", "A"),
            (r"(?ii:a)", "A"),
            (r"(?i-i:a)", "a"),
            (r"a(?i)+", "aaa"),
            (r"a*(?i)+", "aaa"),
            (r"(?)a", "a"),
            (r"(a)", "a"),
            (r"(?:a)", "a"),
            (r"(?P<name>a)", "a"),
            (r"(?P<1>a)", "a"),
            (r"(?P<é>a)", "a"),
            ("(?P<\u{301}>a)", "a"),
            (r"\141", "a"),
            (r"\0", "\0"),
            (r"\a\f\t\n\r\v", "\u{7}\u{c}\t\n\r\u{b}"),
            (r"\x61", "a"),
            (r"\x{E9}", "é"),
            (r"\<\>", "<>"),
            (r"\B", ""),
            (r"\Aa\z", "a"),
            (r"^a$", "a"),
            (r"a\b{start}", "a{start}"),
            (r"\pL", "é"),
            (r"\p{Latin}", "é"),
            (r"\p{Any}", "a"),
            (r"[\p{Any}]", "a"),
            (r"\p{^Latin}", "α"),
            (r"\P{^Latin}", "é"),
            (r"[[:alpha:]]", "A"),
            (r"a{01}", "a{01}"),
            (r"a{,2}", "a{,2}"),
            (r"a{1000000000}", "a{1000000000}"),
        ] {
            assert!(
                matcher
                    .full_match(pattern.as_bytes(), value.as_bytes())
                    .unwrap(),
                "valid RE2 pattern {pattern:?} must match {value:?}"
            );
        }
        assert!(matcher.full_match(br"(?-s:\C)", b"\n").unwrap());
        assert!(!matcher.full_match(br"\P{Any}", b"a").unwrap());
        let thousand_as = "a".repeat(1000);
        assert!(
            matcher
                .full_match(br"a{1000}", thousand_as.as_bytes())
                .unwrap()
        );
        assert!(
            matcher
                .full_match(br"(a{10}){100}", thousand_as.as_bytes())
                .unwrap()
        );

        for pattern in [
            r"(?x)a",
            r"(?u)a",
            r"(?R)a",
            r"(?x:a)",
            r"\p{Alphabetic}",
            r"[\p{Alphabetic}]",
            r"[[:Alphabetic:]]",
            r"\u0061",
            r"\U00000061",
            r"[\u0061]",
            r"(?<name>a)",
            r"(?'name'a)",
            r"(?P<>a)",
            r"(?P<.>a)",
            "(?P<💩>a)",
            r"a{1001}",
            r"a{1,1001}",
            r"(a{11}){91}",
            r"a++",
            r"a{2}*",
            r"\1",
            r"\k<name>",
        ] {
            assert!(
                matcher.full_match(pattern.as_bytes(), b"a").is_err(),
                "Rust-only or invalid RE2 pattern {pattern:?} must fail closed"
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
