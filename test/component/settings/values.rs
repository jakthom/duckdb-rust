use super::*;
use duckdb_rust::{
    DataType,
    main::settings::{Setting, SettingDefinition, SettingRegistry, SettingScope},
    parallel::QueryContext,
    planner::BoundStatement,
};

#[derive(Debug)]
struct ValueSetting {
    data_type: DataType,
    default: Value,
    invalid_normalizer: bool,
}
impl Setting for ValueSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "test_value".into(),
            aliases: vec![],
            data_type: self.data_type.clone(),
            default: self.default.clone(),
            default_scope: SettingScope::Global,
            global: true,
            session: true,
        }
    }
    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.invalid_normalizer && value != &Value::Integer(0) {
            return Ok(Value::Integer(value.as_i128()? + 1));
        }
        Ok(value.clone())
    }
}

fn value_registry(setting: ValueSetting) -> Result<Arc<SettingRegistry>> {
    let mut registry = SettingRegistry::builtins();
    registry.register(Arc::new(setting))?;
    Ok(Arc::new(registry))
}

#[test]
fn floating_settings_preserve_nan_payloads_signed_zero_and_null_across_publication() -> Result<()> {
    for (data_type, values) in [
        (
            DataType::Float,
            vec![
                Value::Float(f32::from_bits(0x7fc0_1234)),
                Value::Float(f32::NAN),
                Value::Float(-0.0),
                Value::Float(0.0),
                Value::Float(f32::INFINITY),
                Value::Float(f32::NEG_INFINITY),
                Value::Null,
            ],
        ),
        (
            DataType::Double,
            vec![
                Value::Double(f64::from_bits(0x7ff8_0000_0000_1234)),
                Value::Double(f64::NAN),
                Value::Double(-0.0),
                Value::Double(0.0),
                Value::Double(f64::INFINITY),
                Value::Double(f64::NEG_INFINITY),
                Value::Null,
            ],
        ),
    ] {
        let default = values[0].clone();
        let registry = value_registry(ValueSetting {
            data_type: data_type.clone(),
            default: default.clone(),
            invalid_normalizer: false,
        })?;
        for provider in super::contracts::providers(&registry) {
            let database = DatabaseBuilder::new().configuration(provider).build()?;
            let mut connection = database.connect();
            let query = QueryContext::background();
            let prepared = connection.prepare("SELECT current_setting('test_value')")?;
            let retained = connection.execute_prepared(&prepared, &[])?;
            for scope in [SettingScope::Global, SettingScope::Session] {
                for value in &values {
                    let change =
                        registry.bind("test_value", Some(scope), Some(value.clone()), &query)?;
                    connection.execute_plan(BoundStatement::Configure(change))?;
                    let result = connection.execute_prepared(&prepared, &[])?;
                    assert_eq!(result.columns[0].data_type, data_type);
                    // The Value serializer retains float bits, including NaN
                    // payloads and signed zero; SQL numeric equality does not.
                    assert_eq!(
                        serde_json::to_value(&result.rows[0][0]).unwrap(),
                        serde_json::to_value(value).unwrap()
                    );
                }
            }
            connection.execute("RESET SESSION test_value; RESET GLOBAL test_value")?;
            assert_eq!(
                serde_json::to_value(&connection.execute_prepared(&prepared, &[])?.rows[0][0])
                    .unwrap(),
                serde_json::to_value(&retained.rows[0][0]).unwrap()
            );
            connection.execute("SET test_value='NaN'")?;
            assert!(
                connection.execute_prepared(&prepared, &[])?.rows[0][0]
                    .as_f64()?
                    .is_nan()
            );
            connection.execute("RESET test_value")?;
        }
    }
    Ok(())
}

#[test]
fn noncanonical_setting_changes_fail_before_publication() -> Result<()> {
    let registry = value_registry(ValueSetting {
        data_type: DataType::BigInt,
        default: Value::Integer(0),
        invalid_normalizer: true,
    })?;
    let query = QueryContext::background();
    for provider in super::contracts::providers(&registry) {
        let mut session = provider.connect();
        let before = session.snapshot(&query)?;
        let change = registry.bind("test_value", None, Some(Value::Integer(4)), &query)?;
        assert!(matches!(
            session.apply(&change, &query),
            Err(Error::Internal(_))
        ));
        assert_eq!(
            session.snapshot(&query)?.get("test_value", &query)?,
            &Value::Integer(0)
        );
        assert_eq!(before.get("test_value", &query)?, &Value::Integer(0));
        let database = DatabaseBuilder::new().configuration(provider).build()?;
        let mut connection = database.connect();
        assert!(matches!(
            connection.execute_plan(BoundStatement::Configure(change)),
            Err(Error::Internal(_))
        ));
        assert_eq!(
            connection
                .query("SELECT current_setting('test_value')")?
                .rows[0][0],
            Value::Integer(0)
        );
    }
    Ok(())
}
