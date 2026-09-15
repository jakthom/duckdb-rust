//! Source-faithful SQLLogicTest control directives.
//!
//! This module deliberately has no dependency on the text parser.  The parser
//! supplies a header implementing [`DirectiveHeader`], and the runner applies
//! the returned action.  That keeps skipped/setup records visible to campaign
//! accounting instead of silently treating them as successful SQL records.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceLocation {
    pub source: String,
    pub line: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.source, self.line)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// The small parser-facing contract. `arguments` must already have the C++
/// parser's whitespace tokenization; quoted explanations remain individual
/// tokens and are joined only where the source does so.
pub(crate) trait DirectiveHeader {
    fn location(&self) -> &SourceLocation;
    fn keyword(&self) -> &str;
    fn arguments(&self) -> &[String];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequirementDisposition {
    Present,
    Skip,
    Fail,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DirectiveAction {
    None,
    SkipFile {
        reason: String,
    },
    Fail {
        message: String,
    },
    SetEnvironment {
        name: String,
        value: String,
    },
    AddTags(Vec<String>),
    SetMode(Mode),
    Sleep(Duration),
    ContinueLoop,
    Include {
        path: String,
        location: SourceLocation,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Mode {
    pub output_hash: bool,
    pub output_result: bool,
    pub debug: bool,
    pub skip_depth: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DirectiveState {
    pub environment: BTreeMap<String, String>,
    pub configured_environment: BTreeSet<String>,
    pub passthrough_environment: BTreeSet<String>,
    pub required_capabilities: BTreeSet<String>,
    pub available_capabilities: BTreeSet<String>,
    pub tags: Vec<String>,
    pub tags_seen: bool,
    pub test_command_seen: bool,
    pub in_loop: bool,
    pub mode: Mode,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DirectiveState {
    pub(crate) fn mark_test_command(&mut self) {
        self.test_command_seen = true;
    }

    fn fail<H: DirectiveHeader>(header: &H, message: impl Into<String>) -> DirectiveAction {
        DirectiveAction::Fail {
            message: format!("{}: {}", header.location(), message.into()),
        }
    }

    /// Handles controls present in both pinned runners. Unknown requirements are
    /// a runner setup failure: accepting them would turn an ineligible test into
    /// a pass. Capability availability is injected by the outer runner.
    pub(crate) fn evaluate<H: DirectiveHeader>(&mut self, header: &H) -> DirectiveAction {
        let args = header.arguments();
        match header.keyword() {
            "require" => {
                if args.is_empty() {
                    return Self::fail(header, "require requires a single parameter");
                }
                let requirement = args[0].to_ascii_lowercase();
                let disposition = if self.available_capabilities.contains(&requirement) {
                    RequirementDisposition::Present
                } else if self.required_capabilities.contains(&requirement) {
                    RequirementDisposition::Fail
                } else {
                    RequirementDisposition::Skip
                };
                match disposition {
                    RequirementDisposition::Present => DirectiveAction::None,
                    RequirementDisposition::Skip => DirectiveAction::SkipFile {
                        reason: format!("require {}", args[0]),
                    },
                    RequirementDisposition::Fail => {
                        Self::fail(header, format!("require {}: FAILED", args[0]))
                    }
                }
            }
            "require-env" => self.require_env(header),
            "test-env" => self.test_env(header),
            "tags" => {
                if self.test_command_seen {
                    return Self::fail(header, "tags expression must precede test commands");
                }
                if self.tags_seen {
                    return Self::fail(header, "tags may be only specified once");
                }
                if args.is_empty() {
                    return Self::fail(header, "tags requires >= 1 argument");
                }
                self.tags_seen = true;
                // Source inserts explicit tags at the front of the implicit list.
                self.tags.splice(0..0, args.iter().cloned());
                DirectiveAction::AddTags(args.to_vec())
            }
            "mode" => self.mode(header),
            "sleep" => self.sleep(header),
            "continue" => {
                if !args.is_empty() {
                    return Self::fail(header, "continue requires no arguments");
                }
                if !self.in_loop {
                    return Self::fail(header, "continue cannot be called outside of a loop");
                }
                DirectiveAction::ContinueLoop
            }
            "include" => {
                if args.len() != 1 {
                    return Self::fail(header, "Expected include <path>");
                }
                DirectiveAction::Include {
                    path: args[0].clone(),
                    location: header.location().clone(),
                }
            }
            _ => Self::fail(
                header,
                format!("unsupported control directive {:?}", header.keyword()),
            ),
        }
    }

    fn test_env<H: DirectiveHeader>(&mut self, header: &H) -> DirectiveAction {
        let args = header.arguments();
        if self.in_loop {
            return Self::fail(header, "test-env cannot be called in a loop");
        }
        if args.len() != 2 {
            return Self::fail(
                header,
                "test-env requires 2 arguments: <env name> <default env val>",
            );
        }
        let value = self
            .environment
            .get(&args[0])
            .cloned()
            .unwrap_or_else(|| args[1].clone());
        self.environment.insert(args[0].clone(), value.clone());
        self.tags.push(format!("env[{}]={value}", args[0]));
        DirectiveAction::SetEnvironment {
            name: args[0].clone(),
            value,
        }
    }

    fn require_env<H: DirectiveHeader>(&mut self, header: &H) -> DirectiveAction {
        let args = header.arguments();
        if self.in_loop {
            return Self::fail(header, "require-env cannot be called in a loop");
        }
        if !(args.len() == 1 || args.len() == 2) {
            return Self::fail(
                header,
                "require-env requires 1 argument: <env name> [optional: <expected env val>]",
            );
        }
        let configured = self.configured_environment.contains(&args[0]);
        let passed = self.passthrough_environment.contains(&args[0]);
        let value = self
            .environment
            .get(&args[0])
            .cloned()
            .or_else(|| std::env::var(&args[0]).ok());
        let Some(value) = value else {
            return DirectiveAction::SkipFile {
                reason: format!("require-env {}", args[0]),
            };
        };
        if args.get(1).is_some_and(|expected| expected != &value) {
            return DirectiveAction::SkipFile {
                reason: format!("require-env {} {}", args[0], args[1]),
            };
        }
        if !configured && !passed && self.environment.contains_key(&args[0]) {
            return Self::fail(
                header,
                format!(
                    "Environment variable '{}' has already been defined",
                    args[0]
                ),
            );
        }
        self.environment.insert(args[0].clone(), value.clone());
        let tag_value = args.get(1).unwrap_or(&value);
        self.tags.push(format!("env[{}]={tag_value}", args[0]));
        DirectiveAction::SetEnvironment {
            name: args[0].clone(),
            value,
        }
    }

    fn mode<H: DirectiveHeader>(&mut self, header: &H) -> DirectiveAction {
        let args = header.arguments();
        if args.is_empty() {
            return Self::fail(header, "mode requires at least one parameter");
        }
        match args[0].as_str() {
            "skip" => self.mode.skip_depth += 1,
            "unskip" => {
                if self.mode.skip_depth == 0 {
                    return Self::fail(header, "mode unskip without matching mode skip");
                }
                self.mode.skip_depth -= 1;
            }
            "output_hash" => self.mode.output_hash = true,
            "output_result" => self.mode.output_result = true,
            "no_output" => {
                self.mode.output_hash = false;
                self.mode.output_result = false;
            }
            "debug" => self.mode.debug = true,
            mode => return Self::fail(header, format!("unrecognized mode: {mode}")),
        }
        DirectiveAction::SetMode(self.mode)
    }

    fn sleep<H: DirectiveHeader>(&mut self, header: &H) -> DirectiveAction {
        let args = header.arguments();
        if args.len() != 2 {
            return Self::fail(header, "sleep requires two parameter (e.g. sleep 1 second)");
        }
        let Ok(count) = args[0].parse::<u64>() else {
            return Self::fail(header, "sleep duration must be an unsigned integer");
        };
        let multiplier = match args[1].as_str() {
            "second" | "seconds" | "sec" => 1_000_000_000,
            "millisecond" | "milliseconds" | "milli" => 1_000_000,
            "microsecond" | "microseconds" | "micro" => 1_000,
            "nanosecond" | "nanoseconds" | "nano" => 1,
            _ => {
                return Self::fail(
                    header,
                    "Unrecognized sleep mode - expected second/millisecond/microsecond/nanosecond",
                );
            }
        };
        let Some(nanos) = count.checked_mul(multiplier) else {
            return Self::fail(header, "sleep duration overflow");
        };
        DirectiveAction::Sleep(Duration::from_nanos(nanos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Header {
        loc: SourceLocation,
        key: String,
        args: Vec<String>,
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Header {
        fn new(key: &str, args: &[&str]) -> Self {
            Self {
                loc: SourceLocation {
                    source: "pin/test.sql".into(),
                    line: 7,
                },
                key: key.into(),
                args: args.iter().map(|s| (*s).into()).collect(),
            }
        }
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl DirectiveHeader for Header {
        fn location(&self) -> &SourceLocation {
            &self.loc
        }
        fn keyword(&self) -> &str {
            &self.key
        }
        fn arguments(&self) -> &[String] {
            &self.args
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn require_env_skip_is_never_a_pass() {
        let mut state = DirectiveState::default();
        assert_eq!(
            state.evaluate(&Header::new("require-env", &["G01_NEVER_SET"])),
            DirectiveAction::SkipFile {
                reason: "require-env G01_NEVER_SET".into()
            }
        );
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn required_missing_capability_fails_but_optional_skips() {
        let mut state = DirectiveState::default();
        assert!(matches!(
            state.evaluate(&Header::new("require", &["parquet"])),
            DirectiveAction::SkipFile { .. }
        ));
        state.required_capabilities.insert("parquet".into());
        assert!(matches!(
            state.evaluate(&Header::new("require", &["parquet"])),
            DirectiveAction::Fail { .. }
        ));
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn development_mode_and_continue_rules_are_explicit() {
        let mut state = DirectiveState::default();
        assert!(matches!(
            state.evaluate(&Header::new("continue", &[])),
            DirectiveAction::Fail { .. }
        ));
        assert!(matches!(
            state.evaluate(&Header::new("mode", &["skip", "reason"])),
            DirectiveAction::SetMode(Mode { skip_depth: 1, .. })
        ));
        assert!(matches!(
            state.evaluate(&Header::new("mode", &["unskip"])),
            DirectiveAction::SetMode(Mode { skip_depth: 0, .. })
        ));
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn tags_and_configured_environment_have_source_dispositions() {
        let mut state = DirectiveState::default();
        state.mark_test_command();
        assert!(matches!(
            state.evaluate(&Header::new("tags", &["slow"])),
            DirectiveAction::Fail { .. }
        ));
        let mut state = DirectiveState::default();
        state.configured_environment.insert("PIN_VALUE".into());
        state.environment.insert("PIN_VALUE".into(), "yes".into());
        assert_eq!(
            state.evaluate(&Header::new("require-env", &["PIN_VALUE", "yes"])),
            DirectiveAction::SetEnvironment {
                name: "PIN_VALUE".into(),
                value: "yes".into()
            }
        );
        assert!(matches!(
            state.evaluate(&Header::new("sleep", &["1", "fortnight"])),
            DirectiveAction::Fail { .. }
        ));
    }
}
