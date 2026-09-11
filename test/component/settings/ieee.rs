use super::*;
use duckdb_rust::{
    DataType,
    common::cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    main::settings::{SettingRegistry, SettingScope},
    parallel::{InterruptHandle, QueryContext},
};

const NAME: &str = "ieee_floating_point_ops";
const SELECT: &str = "SELECT current_setting('ieee_floating_point_ops')";

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_setting_retains_nullable_boolean_metadata_scopes_and_owned_snapshots() -> Result<()> {
    let query = QueryContext::background();
    let registry = Arc::new(SettingRegistry::builtins());
    let definition = registry.definition(NAME)?;
    assert_eq!(definition.data_type, DataType::Boolean);
    assert_eq!(definition.default, Value::Boolean(true));
    assert_eq!(definition.default_scope, SettingScope::Session);
    assert!(definition.global && definition.session);
    for provider in contracts::providers(&registry) {
        let mut a = provider.connect();
        let mut b = provider.connect();
        let initial = a.snapshot(&query)?;
        let change = registry.bind(NAME, None, Some(Value::Boolean(false)), &query)?;
        assert_eq!(change.scope(), SettingScope::Session);
        a.apply(&change, &query)?;
        assert_eq!(
            a.snapshot(&query)?.get(NAME, &query)?,
            &Value::Boolean(false)
        );
        assert_eq!(
            b.snapshot(&query)?.get(NAME, &query)?,
            &Value::Boolean(true)
        );
        b.apply(
            &registry.bind(
                NAME,
                Some(SettingScope::Global),
                Some(Value::Boolean(false)),
                &query,
            )?,
            &query,
        )?;
        a.apply(
            &registry.bind(NAME, None, Some(Value::Null), &query)?,
            &query,
        )?;
        let retained_null = a.snapshot(&query)?;
        assert_eq!(retained_null.get(NAME, &query)?, &Value::Null);
        assert_eq!(
            b.snapshot(&query)?.get(NAME, &query)?,
            &Value::Boolean(false)
        );
        a.apply(&registry.bind(NAME, None, None, &query)?, &query)?;
        assert_eq!(
            a.snapshot(&query)?.get(NAME, &query)?,
            &Value::Boolean(false)
        );
        b.apply(
            &registry.bind(NAME, Some(SettingScope::Global), None, &query)?,
            &query,
        )?;
        assert_eq!(
            a.snapshot(&query)?.get(NAME, &query)?,
            &Value::Boolean(true)
        );
        assert_eq!(initial.get(NAME, &query)?, &Value::Boolean(true));
        assert_eq!(retained_null.get(NAME, &query)?, &Value::Null);
        assert!(
            registry
                .bind(NAME, None, Some(Value::Integer(2)), &query)
                .is_err()
        );
        let interrupt = InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
        interrupt.interrupt();
        assert!(matches!(
            a.apply(&change, &cancelled),
            Err(Error::Interrupted)
        ));
        assert_eq!(
            a.snapshot(&query)?.get(NAME, &query)?,
            &Value::Boolean(true)
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_setting_sql_keeps_selected_casts_nullable_overrides_and_session_lifetimes() -> Result<()> {
    for provider in configurations() {
        let database = DatabaseBuilder::new().configuration(provider).build()?;
        let mut a = database.connect();
        let mut b = database.connect();
        let result = a.query(SELECT)?;
        assert_eq!(result.columns[0].data_type, DataType::Boolean);
        assert_eq!(result.rows, vec![vec![Value::Boolean(true)]]);
        a.execute("SET ieee_floating_point_ops=false")?;
        assert_eq!(a.query(SELECT)?.rows, vec![vec![Value::Boolean(false)]]);
        assert_eq!(b.query(SELECT)?.rows, vec![vec![Value::Boolean(true)]]);
        a.execute("SET GLOBAL ieee_floating_point_ops=false;SET ieee_floating_point_ops=NULL")?;
        assert_eq!(a.query(SELECT)?.rows, vec![vec![Value::Null]]);
        assert_eq!(b.query(SELECT)?.rows, vec![vec![Value::Boolean(false)]]);
        a.execute("RESET ieee_floating_point_ops")?;
        assert_eq!(a.query(SELECT)?.rows, vec![vec![Value::Boolean(false)]]);
        a.execute("BEGIN;SET ieee_floating_point_ops=2;ROLLBACK")?;
        assert_eq!(a.query(SELECT)?.rows, vec![vec![Value::Boolean(true)]]);
        assert!(a.execute("SET ieee_floating_point_ops='bad'").is_err());
        assert_eq!(a.query(SELECT)?.rows, vec![vec![Value::Boolean(true)]]);
        a.execute("RESET ieee_floating_point_ops;RESET GLOBAL ieee_floating_point_ops")?;
        assert_eq!(b.query(SELECT)?.rows, vec![vec![Value::Boolean(true)]]);
        a.execute("SET ieee_floating_point_ops=false")?;
        drop(a);
        assert_eq!(
            database.connect().query(SELECT)?.rows,
            vec![vec![Value::Boolean(true)]]
        );
    }
    Ok(())
}

#[derive(Debug)]
struct BooleanSettingCast(u8);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for BooleanSettingCast {
    fn name(&self) -> &'static str {
        "ieee-setting-selected-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer
            && spec.target == DataType::Boolean
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, _: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match self.0 {
            0 => Ok(Value::Boolean(false)),
            1 => Err(Error::Resource("selected IEEE setting cast budget".into())),
            _ => Ok(Value::Integer(1)),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_setting_uses_selected_boolean_casts_without_publishing_bad_results() -> Result<()> {
    for code in 0..3 {
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: DataType::Integer,
                target: DataType::Boolean,
                mode: CastMode::Explicit,
            },
            Arc::new(BooleanSettingCast(code)),
        )?;
        let mut c = DatabaseBuilder::new().casts(casts).build()?.connect();
        let result = c.execute("SET ieee_floating_point_ops=2");
        match code {
            0 => {
                result?;
            }
            1 => assert!(matches!(result, Err(Error::Resource(_)))),
            _ => assert!(matches!(result, Err(Error::Internal(_)))),
        }
        assert_eq!(c.query(SELECT)?.rows, vec![vec![Value::Boolean(code != 0)]]);
    }
    Ok(())
}
