use super::*;
use crate::catalog::SearchPath;

#[derive(Debug)]
struct OrderingSetting {
    nulls: bool,
}

#[derive(Debug)]
struct SearchPathSetting;

#[derive(Debug)]
struct BooleanControlSetting {
    name: &'static str,
}

#[derive(Debug)]
struct ProfilingFormatSetting;

#[derive(Debug)]
struct ProfilingModeSetting;

#[derive(Debug)]
struct ProfilingOutputSetting;

#[derive(Debug)]
struct MaxMemorySetting;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut SettingRegistry) {
    for name in ["enable_verification", "debug_force_external"] {
        registry
            .register(Arc::new(BooleanControlSetting { name }))
            .expect("unique execution control setting");
    }
    registry
        .register(Arc::new(ProfilingFormatSetting))
        .expect("unique profiling format setting");
    registry
        .register(Arc::new(ProfilingModeSetting))
        .expect("unique profiling mode setting");
    registry
        .register(Arc::new(ProfilingOutputSetting))
        .expect("unique profiling output setting");
    registry
        .register(Arc::new(IeeeFloatingPointSetting))
        .expect("unique IEEE floating point setting");
    registry
        .register(Arc::new(SearchPathSetting))
        .expect("unique search path setting");
    registry
        .register(Arc::new(MaxMemorySetting))
        .expect("unique max memory setting");
    for nulls in [false, true] {
        registry
            .register(Arc::new(OrderingSetting { nulls }))
            .expect("unique ordering setting");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for MaxMemorySetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "max_memory".into(),
            aliases: vec!["memory_limit".into()],
            data_type: DataType::Varchar,
            // The builder resolves its physical-memory default before opening
            // storage. Standalone settings views retain a stable unlimited sentinel.
            default: Value::Varchar("-1".into()),
            default_scope: SettingScope::Global,
            global: true,
            session: false,
        }
    }

    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        let Value::Varchar(text) = value else {
            return Err(Error::InvalidInput("memory_limit must be a VARCHAR".into()));
        };
        let text = text.trim();
        if text.starts_with('-') || matches!(text, "null" | "none") {
            return Ok(Value::Varchar("-1".into()));
        }
        let lower = text.to_ascii_lowercase();
        if let Some(number) = lower.strip_suffix('%') {
            let value = number.trim().parse::<f64>().map_err(|_| {
                Error::InvalidInput("memory_limit has an invalid percentage".into())
            })?;
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(Error::InvalidInput(
                    "memory_limit percentage must be between 0 and 100".into(),
                ));
            }
            // Preserve the lexical percentage as typed setting state. The
            // selected database provider resolves it only when publishing.
            return Ok(Value::Varchar(format!("{}%", value as usize)));
        }
        let number_end = lower
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'-'))
            .count();
        let number = &lower[..number_end];
        let unit = lower[number_end..].split_whitespace().next().unwrap_or("");
        let multiplier = match unit {
            "byte" | "bytes" | "b" => 1usize,
            "kilobyte" | "kilobytes" | "kb" | "k" => 1000usize,
            "megabyte" | "megabytes" | "mb" | "m" => 1000usize.pow(2),
            "gigabyte" | "gigabytes" | "gb" | "g" => 1000usize.pow(3),
            "terabyte" | "terabytes" | "tb" | "t" => 1000usize.pow(4),
            "kib" => 1024usize,
            "mib" => 1024usize.pow(2),
            "gib" => 1024usize.pow(3),
            "tib" => 1024usize.pow(4),
            _ => {
                return Err(Error::InvalidInput(
                    "memory_limit requires a recognized byte unit".into(),
                ));
            }
        };
        let number = number
            .parse::<f64>()
            .map_err(|_| Error::InvalidInput("memory_limit has an invalid byte value".into()))?;
        if !number.is_finite() || number < 0.0 {
            return Err(Error::InvalidInput(
                "memory_limit has an invalid byte value".into(),
            ));
        }
        let bytes = number * multiplier as f64;
        if bytes > usize::MAX as f64 {
            return Err(Error::Resource(
                "memory_limit exceeds addressable memory".into(),
            ));
        }
        Ok(Value::Varchar(format!("{}B", bytes as usize)))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::main) fn max_memory_bytes(
    value: &Value,
    base: &dyn super::host_memory::MemoryLimitBase,
) -> Result<Option<usize>> {
    match value {
        Value::Varchar(value) if value == "-1" => Ok(None),
        Value::Varchar(value) if value.ends_with('%') => {
            let percentage = value[..value.len() - 1]
                .parse::<usize>()
                .map_err(|_| Error::Internal("invalid normalized max_memory percentage".into()))?;
            let total = base.base_bytes()?;
            Ok(Some(((total as u128 * percentage as u128) / 100) as usize))
        }
        Value::Varchar(value) => value
            .strip_suffix('B')
            .ok_or_else(|| Error::Internal("invalid normalized max_memory setting".into()))?
            .parse()
            .map(Some)
            .map_err(|_| Error::Internal("invalid normalized max_memory setting".into())),
        _ => Err(Error::Internal("invalid max_memory setting type".into())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::main) fn default_memory_limit(
    base: &dyn super::host_memory::MemoryLimitBase,
) -> Result<Option<usize>> {
    let bytes = base.base_bytes()?;
    if bytes == usize::MAX {
        return Ok(None);
    }
    Ok(Some(if base.fallback() {
        bytes
    } else {
        (bytes as u128 * 8 / 10) as usize
    }))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn format_memory_limit(value: &Value) -> Result<Value> {
    let bytes = match value {
        Value::Varchar(value) if value == "-1" => usize::MAX,
        Value::Varchar(value) => value
            .strip_suffix('B')
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| Error::Internal("memory limit is not resolved".into()))?,
        _ => return Err(Error::Internal("invalid memory limit type".into())),
    };
    let mut parts = [0usize; 6];
    parts[0] = bytes;
    for index in 1..parts.len() {
        parts[index] = parts[index - 1] / 1024;
        parts[index - 1] %= 1024;
    }
    let units = ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB"];
    for index in (1..parts.len()).rev() {
        if parts[index] != 0 {
            return Ok(Value::Varchar(format!(
                "{}.{} {}",
                parts[index],
                parts[index - 1] * 10 / 1024,
                units[index]
            )));
        }
    }
    Ok(Value::Varchar(format!(
        "{bytes} {}",
        if bytes == 1 { "byte" } else { "bytes" }
    )))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for BooleanControlSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: self.name.into(),
            aliases: Vec::new(),
            data_type: DataType::Boolean,
            default: Value::Boolean(false),
            default_scope: SettingScope::Session,
            global: false,
            session: true,
        }
    }

    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            Value::Boolean(_) => Ok(value.clone()),
            Value::Null => Err(Error::InvalidInput(format!(
                "{} must be a non-NULL BOOLEAN",
                self.name
            ))),
            _ => Err(Error::Internal(format!(
                "{} setting input was not BOOLEAN",
                self.name
            ))),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for ProfilingFormatSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "enable_profiling".into(),
            aliases: vec!["enable_profile".into()],
            data_type: DataType::Varchar,
            default: Value::Null,
            default_scope: SettingScope::Session,
            global: false,
            session: true,
        }
    }

    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        let Value::Varchar(value) = value else {
            if value == &Value::Null {
                return Ok(Value::Null);
            }
            return Err(Error::Internal(
                "enable_profiling setting input was not VARCHAR".into(),
            ));
        };
        let value = value.to_ascii_lowercase();
        match value.as_str() {
            "default"
            | "text"
            | "query_tree"
            | "query_tree_optimizer"
            | "no_output"
            | "json"
            | "html"
            | "graphviz"
            | "yaml"
            | "mermaid" => Ok(Value::Varchar(value)),
            _ => Err(Error::InvalidInput(format!(
                "\"{value}\" is not a valid FORMAT argument, valid options are: default, text, query_tree, query_tree_optimizer, no_output, json, html, graphviz, yaml, mermaid"
            ))),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for ProfilingModeSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "profiling_mode".into(),
            aliases: Vec::new(),
            data_type: DataType::Varchar,
            default: Value::Null,
            default_scope: SettingScope::Session,
            global: false,
            session: true,
        }
    }

    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        let Value::Varchar(value) = value else {
            if value == &Value::Null {
                return Ok(Value::Null);
            }
            return Err(Error::Internal(
                "profiling_mode setting input was not VARCHAR".into(),
            ));
        };
        let value = value.to_ascii_lowercase();
        match value.as_str() {
            // Development accepts all three spellings but always gathers the
            // detailed information and reports the effective mode as standard.
            "standard" | "detailed" | "all" => Ok(Value::Varchar("standard".into())),
            _ => Err(Error::Parse(format!(
                "Unrecognized profiling mode \"{value}\", supported formats: [standard, detailed, all]"
            ))),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for ProfilingOutputSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "profiling_output".into(),
            aliases: vec!["profile_output".into()],
            data_type: DataType::Varchar,
            default: Value::Varchar(String::new()),
            default_scope: SettingScope::Session,
            global: false,
            session: true,
        }
    }

    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            Value::Varchar(_) => Ok(value.clone()),
            Value::Null => Err(Error::InvalidInput(
                "profiling_output must be a non-NULL VARCHAR".into(),
            )),
            _ => Err(Error::Internal(
                "profiling_output setting input was not VARCHAR".into(),
            )),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for SearchPathSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "search_path".into(),
            aliases: vec![],
            data_type: DataType::Varchar,
            default: Value::Varchar(String::new()),
            default_scope: SettingScope::Session,
            global: false,
            session: true,
        }
    }

    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        let Value::Varchar(value) = value else {
            return Err(Error::InvalidInput(
                "search_path must be a non-NULL VARCHAR".into(),
            ));
        };
        let path = SearchPath::from_setting(value)?;
        query.check()?;
        Ok(Value::Varchar(path.to_string()))
    }
}

#[derive(Debug)]
struct IeeeFloatingPointSetting;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for IeeeFloatingPointSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "ieee_floating_point_ops".into(),
            aliases: vec![],
            data_type: DataType::Boolean,
            default: Value::Boolean(true),
            default_scope: SettingScope::Session,
            global: true,
            session: true,
        }
    }
    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            // NULL remains observable in current_setting. The consuming native
            // typed getter uses the default without rewriting stored metadata.
            Value::Boolean(_) | Value::Null => Ok(value.clone()),
            _ => Err(Error::Internal("IEEE setting input was not BOOLEAN".into())),
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Setting for OrderingSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: if self.nulls {
                "default_null_order"
            } else {
                "default_order"
            }
            .into(),
            aliases: if self.nulls {
                vec!["null_order".into()]
            } else {
                vec![]
            },
            data_type: DataType::Varchar,
            default: Value::Varchar(
                if self.nulls {
                    "NULLS_LAST"
                } else {
                    "ASCENDING"
                }
                .into(),
            ),
            default_scope: SettingScope::Global,
            global: true,
            session: true,
        }
    }
    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        let parameter = value.to_string().to_ascii_lowercase();
        let normalized = if self.nulls {
            match parameter.as_str() {
                "nulls_first" | "nulls first" | "null first" | "first" => "NULLS_FIRST",
                "nulls_last" | "nulls last" | "null last" | "last" => "NULLS_LAST",
                "nulls_first_on_asc_last_on_desc" | "sqlite" | "mysql" => {
                    "NULLS_FIRST_ON_ASC_LAST_ON_DESC"
                }
                "nulls_last_on_asc_first_on_desc" | "postgres" => "NULLS_LAST_ON_ASC_FIRST_ON_DESC",
                _ => {
                    return Err(Error::Parse(format!(
                        "Unrecognized parameter for option NULL_ORDER \"{parameter}\", expected either NULLS FIRST, NULLS LAST, SQLite, MySQL or Postgres"
                    )));
                }
            }
        } else {
            match parameter.as_str() {
                "ascending" | "asc" => "ASC",
                "descending" | "desc" => "DESC",
                _ => {
                    return Err(Error::Bind(format!(
                        "Unrecognized parameter for option DEFAULT_ORDER \"{parameter}\". Expected ASC or DESC."
                    )));
                }
            }
        };
        Ok(Value::Varchar(normalized.into()))
    }
}
