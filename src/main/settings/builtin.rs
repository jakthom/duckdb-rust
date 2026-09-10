use super::*;

#[derive(Debug)]
struct OrderingSetting {
    nulls: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut SettingRegistry) {
    for nulls in [false, true] {
        registry
            .register(Arc::new(OrderingSetting { nulls }))
            .expect("unique ordering setting");
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
