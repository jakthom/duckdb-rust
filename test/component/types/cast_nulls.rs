use super::*;
use duckdb_rust::common::{
    cast::{CastFunction, CastNullHandling},
    vector::Vector,
};

#[derive(Debug)]
struct NullAwareCast {
    drop_nonnull: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for NullAwareCast {
    fn name(&self) -> &'static str {
        "typed-null-test-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer && spec.target == DataType::Varchar
    }
    fn null_handling(&self, _: &CastSpec) -> CastNullHandling {
        CastNullHandling::Call
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value.is_null() {
            Ok(Value::Varchar("active NULL".into()))
        } else if self.drop_nonnull {
            Ok(Value::Null)
        } else {
            Ok(Value::Varchar(value.to_string()))
        }
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn selected_casts_can_handle_typed_nulls_without_hiding_invalid_nonnull_output() -> Result<()> {
    let query = QueryContext::background();
    let spec = CastSpec {
        source: DataType::Integer,
        target: DataType::Varchar,
        mode: CastMode::Explicit,
    };
    for drop_nonnull in [false, true] {
        let mut registry = CastRegistry::default();
        registry.register(spec.clone(), Arc::new(NullAwareCast { drop_nonnull }))?;
        let bound = registry.bind(&spec.source, &spec.target, spec.mode, query.types())?;
        assert_eq!(
            bound.apply(&Value::Null, &query)?,
            Value::Varchar("active NULL".into())
        );
        let vector = Vector::flat(DataType::Integer, vec![Value::Integer(42), Value::Null])?;
        if drop_nonnull {
            assert!(matches!(
                bound.apply(&Value::Integer(42), &query),
                Err(Error::Internal(_))
            ));
            assert!(matches!(
                bound.apply_batch(&vector, &query),
                Err(Error::Internal(_))
            ));
        } else {
            assert_eq!(
                bound
                    .apply_batch(&vector, &query)?
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![
                    Value::Varchar("42".into()),
                    Value::Varchar("active NULL".into())
                ]
            );
        }
    }
    let ordinary =
        CastRegistry::builtins().bind(&spec.source, &spec.target, spec.mode, query.types())?;
    assert_eq!(ordinary.apply(&Value::Null, &query)?, Value::Null);
    Ok(())
}
