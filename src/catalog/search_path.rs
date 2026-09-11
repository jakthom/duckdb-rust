//! Pure catalog search-path syntax and resolution ordering.
//!
//! Session catalog validation and setting application are deliberately outside
//! this value model. Construction completes before replacement so malformed or
//! over-budget input cannot partially update a live path.

use std::fmt;
use std::str::FromStr;

use crate::common::{Error, Result};

const DEFAULT_SCHEMA: &str = "main";
const TEMP_CATALOG: &str = "temp";
const SYSTEM_CATALOG: &str = "system";
const PG_CATALOG_SCHEMA: &str = "pg_catalog";
const MAX_SETTING_BYTES: usize = 65_536;
const MAX_IDENTIFIER_BYTES: usize = 4_096;
const MAX_ENTRIES: usize = 4_096;

#[derive(Clone, Debug)]
struct Identifier(String);

impl Identifier {
    fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(Error::InvalidInput(
                "search path identifier cannot be empty".into(),
            ));
        }
        if value.len() > MAX_IDENTIFIER_BYTES {
            return Err(Error::Resource(
                "search path identifier exceeds size limit".into(),
            ));
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Identifier {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Eq for Identifier {}

/// One schema search location. An absent catalog is resolved against the
/// caller's current default catalog; qualification is otherwise retained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPathEntry {
    catalog: Option<Identifier>,
    schema: Identifier,
}

impl SearchPathEntry {
    pub fn schema(schema: impl Into<String>) -> Result<Self> {
        Ok(Self {
            catalog: None,
            schema: Identifier::new(schema)?,
        })
    }

    pub fn qualified(catalog: impl Into<String>, schema: impl Into<String>) -> Result<Self> {
        Ok(Self {
            catalog: Some(Identifier::new(catalog)?),
            schema: Identifier::new(schema)?,
        })
    }

    pub fn parse(input: &str) -> Result<Self> {
        let mut entries = parse_entries(input)?;
        if entries.len() != 1 {
            return Err(Error::Parse(format!(
                "failed to convert {input:?} to one search path entry"
            )));
        }
        Ok(entries.remove(0))
    }

    pub fn catalog(&self) -> Option<&str> {
        self.catalog.as_ref().map(Identifier::as_str)
    }

    pub fn schema_name(&self) -> &str {
        self.schema.as_str()
    }

    fn in_catalog(&self, default_catalog: &Identifier) -> Result<Self> {
        match &self.catalog {
            Some(_) => Ok(self.clone()),
            None => Self::qualified(default_catalog.as_str(), self.schema.as_str()),
        }
    }
}

impl fmt::Display for SearchPathEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(catalog) = &self.catalog {
            write_identifier(formatter, catalog.as_str())?;
            formatter.write_str(".")?;
        }
        write_identifier(formatter, self.schema.as_str())
    }
}

impl FromStr for SearchPathEntry {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        Self::parse(input)
    }
}

/// A configured search path. `None` is the inherited/default path, while
/// `Some([])` is an explicitly empty setting; both currently resolve through
/// the implicit temp/main/system entries but remain observably distinct.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SearchPath {
    explicit: Option<Vec<SearchPathEntry>>,
}

impl SearchPath {
    pub fn explicit(entries: Vec<SearchPathEntry>) -> Result<Self> {
        validate_entry_count(entries.len())?;
        Ok(Self {
            explicit: Some(entries),
        })
    }

    pub fn from_setting(input: &str) -> Result<Self> {
        Self::explicit(parse_entries(input)?)
    }

    pub fn is_explicit(&self) -> bool {
        self.explicit.is_some()
    }

    /// Only the user-supplied setting entries. Default and explicitly empty
    /// paths both return an empty slice; `is_explicit` distinguishes them.
    pub fn entries(&self) -> &[SearchPathEntry] {
        self.explicit.as_deref().unwrap_or_default()
    }

    pub fn explicit_entries(&self) -> Option<&[SearchPathEntry]> {
        self.explicit.as_deref()
    }

    /// The schema used for unqualified creation and current_schema(). The
    /// implicit temporary entry never becomes the default schema.
    pub fn current_schema(&self) -> &str {
        self.entries()
            .first()
            .map(SearchPathEntry::schema_name)
            .unwrap_or(DEFAULT_SCHEMA)
    }

    /// Expand the pure lookup order for a caller-selected persistent catalog.
    /// Duplicates are intentional and retain the configured order, matching
    /// DuckDB's search path rather than silently canonicalizing a set.
    pub fn resolution_candidates(&self, default_catalog: &str) -> Result<Vec<SearchPathEntry>> {
        let default_catalog = Identifier::new(default_catalog)?;
        let capacity = self
            .entries()
            .len()
            .checked_add(4)
            .ok_or_else(|| Error::Resource("search path candidate count overflow".into()))?;
        let mut candidates = Vec::new();
        candidates
            .try_reserve(capacity)
            .map_err(|_| Error::Resource("search path candidate allocation".into()))?;
        candidates.push(SearchPathEntry::qualified(TEMP_CATALOG, DEFAULT_SCHEMA)?);
        for entry in self.entries() {
            candidates.push(entry.in_catalog(&default_catalog)?);
        }
        candidates.push(SearchPathEntry::qualified(
            default_catalog.as_str(),
            DEFAULT_SCHEMA,
        )?);
        candidates.push(SearchPathEntry::qualified(SYSTEM_CATALOG, DEFAULT_SCHEMA)?);
        candidates.push(SearchPathEntry::qualified(
            SYSTEM_CATALOG,
            PG_CATALOG_SCHEMA,
        )?);
        Ok(candidates)
    }

    /// Parse a complete replacement first. On error `self` is unchanged.
    pub fn update_from_setting(&mut self, input: &str) -> Result<()> {
        let replacement = Self::from_setting(input)?;
        *self = replacement;
        Ok(())
    }

    /// Validate a complete replacement first. On error `self` is unchanged.
    pub fn update_entries(&mut self, entries: Vec<SearchPathEntry>) -> Result<()> {
        let replacement = Self::explicit(entries)?;
        *self = replacement;
        Ok(())
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl fmt::Display for SearchPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, entry) in self.entries().iter().enumerate() {
            if index != 0 {
                formatter.write_str(",")?;
            }
            entry.fmt(formatter)?;
        }
        Ok(())
    }
}

impl FromStr for SearchPath {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        Self::from_setting(input)
    }
}

fn validate_entry_count(count: usize) -> Result<()> {
    if count > MAX_ENTRIES {
        return Err(Error::Resource(
            "search path exceeds entry count limit".into(),
        ));
    }
    Ok(())
}

fn parse_entries(input: &str) -> Result<Vec<SearchPathEntry>> {
    if input.len() > MAX_SETTING_BYTES {
        return Err(Error::Resource(
            "search path setting exceeds size limit".into(),
        ));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    entries
        .try_reserve(input.len().min(MAX_ENTRIES))
        .map_err(|_| Error::Resource("search path entry allocation".into()))?;
    let mut components = Vec::with_capacity(2);
    let mut component = String::new();
    let mut quoted = false;
    let mut characters = input.chars().peekable();

    while let Some(character) = characters.next() {
        match character {
            '"' if quoted && characters.peek() == Some(&'"') => {
                characters.next();
                push_identifier_character(&mut component, '"')?;
            }
            '"' => quoted = !quoted,
            '.' if !quoted => finish_component(&mut components, &mut component)?,
            ',' if !quoted => {
                finish_entry(&mut entries, &mut components, &mut component)?;
                validate_entry_count(entries.len())?;
            }
            character => push_identifier_character(&mut component, character)?,
        }
    }
    if quoted {
        return Err(Error::Parse(
            "unterminated quote in qualified search path name".into(),
        ));
    }
    // A trailing comma terminates the preceding entry and does not create an
    // additional empty one in both pinned implementations.
    if !component.is_empty() || !components.is_empty() {
        finish_entry(&mut entries, &mut components, &mut component)?;
    }
    validate_entry_count(entries.len())?;
    Ok(entries)
}

fn push_identifier_character(component: &mut String, character: char) -> Result<()> {
    let size = component
        .len()
        .checked_add(character.len_utf8())
        .ok_or_else(|| Error::Resource("search path identifier size overflow".into()))?;
    if size > MAX_IDENTIFIER_BYTES {
        return Err(Error::Resource(
            "search path identifier exceeds size limit".into(),
        ));
    }
    component.push(character);
    Ok(())
}

fn finish_component(components: &mut Vec<String>, component: &mut String) -> Result<()> {
    if component.is_empty() {
        return Err(Error::Parse(
            "unexpected separator: empty search path entry".into(),
        ));
    }
    if components.len() == 2 {
        return Err(Error::Parse(
            "too many dots: expected schema or catalog.schema".into(),
        ));
    }
    components.push(std::mem::take(component));
    Ok(())
}

fn finish_entry(
    entries: &mut Vec<SearchPathEntry>,
    components: &mut Vec<String>,
    component: &mut String,
) -> Result<()> {
    finish_component(components, component)?;
    let entry = match components.len() {
        1 => SearchPathEntry::schema(components.remove(0))?,
        2 => {
            let schema = components.pop().expect("two components");
            let catalog = components.pop().expect("two components");
            SearchPathEntry::qualified(catalog, schema)?
        }
        _ => return Err(Error::Internal("search path component count".into())),
    };
    entries.push(entry);
    Ok(())
}

fn write_identifier(formatter: &mut fmt::Formatter<'_>, identifier: &str) -> fmt::Result {
    if !identifier
        .bytes()
        .any(|byte| matches!(byte, b'.' | b',' | b'"'))
    {
        return formatter.write_str(identifier);
    }
    formatter.write_str("\"")?;
    for character in identifier.chars() {
        if character == '"' {
            formatter.write_str("\"\"")?;
        } else {
            write!(formatter, "{character}")?;
        }
    }
    formatter.write_str("\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(entries: &[SearchPathEntry]) -> Vec<(Option<&str>, &str)> {
        entries
            .iter()
            .map(|entry| (entry.catalog(), entry.schema_name()))
            .collect()
    }

    #[test]
    fn setting_parser_preserves_spelling_quotes_and_round_trips() {
        let input = "Sales,\"Mixed.Case\",\"a,b\",\"a\"\"b\",catalog.schema,\"cat.with.dot\".\"schema,with,comma\"";
        let path = SearchPath::from_setting(input).unwrap();
        assert_eq!(
            names(path.entries()),
            vec![
                (None, "Sales"),
                (None, "Mixed.Case"),
                (None, "a,b"),
                (None, "a\"b"),
                (Some("catalog"), "schema"),
                (Some("cat.with.dot"), "schema,with,comma"),
            ]
        );
        assert_eq!(path.to_string(), input);
        assert_eq!(SearchPath::from_setting(&path.to_string()).unwrap(), path);

        let upper = SearchPathEntry::parse("MAIN").unwrap();
        let quoted_lower = SearchPathEntry::parse("\"main\"").unwrap();
        assert_eq!(upper, quoted_lower);
        assert_eq!(upper.to_string(), "MAIN");
        assert_eq!(quoted_lower.to_string(), "main");
        assert_eq!(
            SearchPathEntry::parse("MEMORY.MAIN").unwrap(),
            SearchPathEntry::parse("memory.main").unwrap()
        );
        assert_eq!(
            SearchPathEntry::parse("a,").unwrap(),
            SearchPathEntry::schema("A").unwrap()
        );
    }

    #[test]
    fn default_explicit_and_resolution_order_remain_distinct() {
        let inherited = SearchPath::default();
        assert!(!inherited.is_explicit());
        assert_eq!(inherited.explicit_entries(), None);
        assert_eq!(inherited.current_schema(), "main");
        assert_eq!(inherited.to_string(), "");
        assert_eq!(
            names(&inherited.resolution_candidates("memory").unwrap()),
            vec![
                (Some("temp"), "main"),
                (Some("memory"), "main"),
                (Some("system"), "main"),
                (Some("system"), "pg_catalog"),
            ]
        );

        let empty = SearchPath::from_setting("").unwrap();
        assert!(empty.is_explicit());
        assert!(empty.explicit_entries().is_some_and(<[_]>::is_empty));
        assert_ne!(empty, inherited);
        assert_eq!(empty.current_schema(), "main");

        let configured = SearchPath::from_setting("Analytics,other.Events,analytics").unwrap();
        assert_eq!(configured.current_schema(), "Analytics");
        assert_eq!(
            names(&configured.resolution_candidates("memory").unwrap()),
            vec![
                (Some("temp"), "main"),
                (Some("memory"), "Analytics"),
                (Some("other"), "Events"),
                (Some("memory"), "analytics"),
                (Some("memory"), "main"),
                (Some("system"), "main"),
                (Some("system"), "pg_catalog"),
            ]
        );
        assert_eq!(configured.entries()[0], configured.entries()[2]);
        assert_eq!(configured.to_string(), "Analytics,other.Events,analytics");

        let explicit_temp = SearchPath::from_setting("temp.main,main").unwrap();
        assert_eq!(
            names(&explicit_temp.resolution_candidates("memory").unwrap())[..3],
            [
                (Some("temp"), "main"),
                (Some("temp"), "main"),
                (Some("memory"), "main"),
            ]
        );
    }

    #[test]
    fn invalid_syntax_is_rejected_without_partial_update() {
        for input in [",main", "main,,other", ".main", "main.", "a.b.c", "\"main"] {
            assert!(
                matches!(SearchPath::from_setting(input), Err(Error::Parse(_))),
                "{input:?}"
            );
        }
        for input in ["", "\"\"", "main,other"] {
            assert!(
                matches!(SearchPathEntry::parse(input), Err(Error::Parse(_))),
                "{input:?}"
            );
        }

        let mut path = SearchPath::from_setting("first,second").unwrap();
        let original = path.clone();
        assert!(
            path.update_from_setting("replacement,\"unterminated")
                .is_err()
        );
        assert_eq!(path, original);
        path.update_from_setting("replacement,main").unwrap();
        assert_eq!(path.to_string(), "replacement,main");
        path.reset();
        assert_eq!(path, SearchPath::default());
    }

    #[test]
    fn whitespace_duplicates_and_optional_quoting_match_the_pins() {
        let path =
            SearchPath::from_setting("Main,main, lead,\"comma,name\",\"dot.name\",\"a\"\"b\"")
                .unwrap();
        assert_eq!(path.entries().len(), 6);
        assert_eq!(path.entries()[0], path.entries()[1]);
        assert_eq!(path.entries()[2].schema_name(), " lead");
        assert_eq!(
            path.to_string(),
            "Main,main, lead,\"comma,name\",\"dot.name\",\"a\"\"b\""
        );
    }

    #[test]
    fn parser_enforces_identifier_setting_and_entry_bounds() {
        assert!(matches!(
            SearchPathEntry::schema(""),
            Err(Error::InvalidInput(_))
        ));
        let maximal = "x".repeat(MAX_IDENTIFIER_BYTES);
        assert_eq!(
            SearchPathEntry::schema(maximal.clone())
                .unwrap()
                .schema_name()
                .len(),
            MAX_IDENTIFIER_BYTES
        );
        assert!(matches!(
            SearchPathEntry::schema(format!("{maximal}x")),
            Err(Error::Resource(_))
        ));
        assert!(matches!(
            SearchPath::from_setting(&"x".repeat(MAX_SETTING_BYTES + 1)),
            Err(Error::Resource(_))
        ));

        let maximum_entries = std::iter::repeat_n("x", MAX_ENTRIES)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            SearchPath::from_setting(&maximum_entries)
                .unwrap()
                .entries()
                .len(),
            MAX_ENTRIES
        );
        let too_many_entries = format!("{maximum_entries},x");
        assert!(matches!(
            SearchPath::from_setting(&too_many_entries),
            Err(Error::Resource(_))
        ));

        let mut path = SearchPath::from_setting("original").unwrap();
        let original = path.clone();
        let entry = SearchPathEntry::schema("x").unwrap();
        assert!(matches!(
            path.update_entries(vec![entry; MAX_ENTRIES + 1]),
            Err(Error::Resource(_))
        ));
        assert_eq!(path, original);
    }
}
