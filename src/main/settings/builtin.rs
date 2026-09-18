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
    for nulls in [false, true] {
        registry
            .register(Arc::new(OrderingSetting { nulls }))
            .expect("unique ordering setting");
    }
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
