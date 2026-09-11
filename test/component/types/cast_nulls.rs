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

#[derive(Debug)]
struct NullableCast {
    nullable: bool,
    call_nulls: bool,
    malformed_batch: bool,
    output: Value,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for NullableCast {
    fn name(&self) -> &'static str {
        "nullable-test-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer && spec.target == DataType::Varchar
    }
    fn null_handling(&self, _: &CastSpec) -> CastNullHandling {
        if self.call_nulls {
            CastNullHandling::Call
        } else {
            CastNullHandling::Propagate
        }
    }
    fn may_return_null(&self, _: &CastSpec) -> bool {
        self.nullable
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value.is_null() {
            Ok(Value::Varchar("active NULL".into()))
        } else {
            Ok(self.output.clone())
        }
    }
    fn cast_batch(&self, input: &Vector, spec: &CastSpec, query: &QueryContext) -> Result<Vector> {
        let values = input
            .values()
            .map(|value| {
                if value.is_null() && !self.call_nulls && !self.malformed_batch {
                    Ok(Value::Null)
                } else {
                    self.cast(value, spec, query)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Vector::flat(spec.target.clone(), values)
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn selected_output_nullability_is_independent_retained_and_checked() -> Result<()> {
    let query = QueryContext::background();
    let spec = CastSpec {
        source: DataType::Integer,
        target: DataType::Varchar,
        mode: CastMode::Explicit,
    };
    let input = Vector::flat(DataType::Integer, vec![Value::Integer(7), Value::Null])?;
    for call_nulls in [false, true] {
        let mut registry = CastRegistry::default();
        registry.register(
            spec.clone(),
            Arc::new(NullableCast {
                nullable: true,
                call_nulls,
                malformed_batch: false,
                output: Value::Null,
            }),
        )?;
        let retained = registry.bind(&spec.source, &spec.target, spec.mode, query.types())?;
        let expected = vec![
            Value::Null,
            if call_nulls {
                Value::Varchar("active NULL".into())
            } else {
                Value::Null
            },
        ];
        registry.replace(
            spec.clone(),
            Arc::new(NullableCast {
                nullable: false,
                call_nulls,
                malformed_batch: false,
                output: Value::Null,
            }),
        )?;
        assert_eq!(retained.apply(&Value::Integer(7), &query)?, Value::Null);
        assert_eq!(retained.apply(&Value::Null, &query)?, expected[1]);
        assert_eq!(
            retained
                .apply_batch(&input, &query)?
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            expected
        );
        let replacement = registry.bind(&spec.source, &spec.target, spec.mode, query.types())?;
        assert!(matches!(
            replacement.apply(&Value::Integer(7), &query),
            Err(Error::Internal(_))
        ));
        assert!(matches!(
            replacement.apply_batch(&input, &query),
            Err(Error::Internal(_))
        ));
    }
    for (output, malformed_batch) in [(Value::Integer(3), false), (Value::Null, true)] {
        let mut registry = CastRegistry::default();
        registry.register(
            spec.clone(),
            Arc::new(NullableCast {
                nullable: true,
                call_nulls: false,
                malformed_batch,
                output,
            }),
        )?;
        let bound = registry.bind(&spec.source, &spec.target, spec.mode, query.types())?;
        if !malformed_batch {
            assert!(matches!(
                bound.apply(&Value::Integer(7), &query),
                Err(Error::Internal(_))
            ));
        }
        assert!(matches!(
            bound.apply_batch(&input, &query),
            Err(Error::Internal(_))
        ));
    }
    Ok(())
}
