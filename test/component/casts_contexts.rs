use super::*;
use duckdb_rust::{
    common::{
        NestedType,
        cast::{CastBehavior, CastFailure, CastResult, CastSourceContext},
    },
    execution::expression_executor::BatchedEvaluator,
    optimizer::IdentityOptimizer,
};

#[derive(Debug)]
struct OrdinaryOnly;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for OrdinaryOnly {
    fn name(&self) -> &'static str {
        "ordinary-only-source-context"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar && spec.target == DataType::Integer
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        Ok(Value::Integer(11))
    }
}

#[derive(Debug)]
struct ContextAware;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for ContextAware {
    fn name(&self) -> &'static str {
        "context-aware-source-context"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        OrdinaryOnly.supports(spec)
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        OrdinaryOnly.cast(value, spec, query)
    }
    fn cast_attempt_with_context(
        &self,
        value: &Value,
        spec: &CastSpec,
        behavior: CastBehavior,
        source_context: CastSourceContext,
        query: &QueryContext,
    ) -> CastResult<Value> {
        if source_context == CastSourceContext::Ordinary {
            return self.cast_attempt(value, spec, behavior, query);
        }
        match value {
            Value::Varchar(text) if text == "invalid" => Err(CastFailure::invalid_input(
                Error::Conversion("context input rejected".into()),
            )),
            Value::Varchar(text) if text == "resource" => Err(CastFailure::invalid_input(
                Error::Resource("context resource witness".into()),
            )),
            Value::Varchar(text) if text == "internal" => Err(CastFailure::fatal(Error::Internal(
                "context internal witness".into(),
            ))),
            Value::Varchar(text) if text == "malformed" => {
                Ok(Value::Varchar("not an integer".into()))
            }
            Value::Varchar(text) if text == "null" => Ok(Value::Null),
            _ => Ok(Value::Integer(22)),
        }
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn variant_source_context_keeps_selected_replacements_and_whole_value_try_failures() -> Result<()> {
    for aware in [false, true] {
        for batched in [false, true] {
            let mut casts = CastRegistry::builtins();
            casts.replace(
                spec(DataType::Integer, CastMode::Explicit),
                if aware {
                    Arc::new(ContextAware)
                } else {
                    Arc::new(OrdinaryOnly)
                },
            )?;
            let query = QueryContext::background();
            let variant = NestedType::Variant.data_type();
            let retained = casts.bind(
                &variant,
                &DataType::Integer,
                CastMode::Explicit,
                query.types(),
            )?;
            let wrap = casts.bind(
                &DataType::Varchar,
                &variant,
                CastMode::Explicit,
                query.types(),
            )?;
            let value = wrap.apply(&Value::Varchar("value".into()), &query)?;
            let mut c = DatabaseBuilder::new()
                .casts(casts.clone())
                .optimizer(Arc::new(IdentityOptimizer))
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .batch_size(2)
                .build()?
                .connect();
            casts.replace(
                spec(DataType::Integer, CastMode::Explicit),
                Arc::new(PrimitiveCast),
            )?;
            let expected = if aware { 22 } else { 11 };
            assert_eq!(retained.apply(&value, &query)?, Value::Integer(expected));
            let rows = c.query("SELECT CAST(v AS INTEGER),CAST(v::VARIANT AS INTEGER) FROM (VALUES('a'),('b'),(NULL),('c')) t(v)")?.rows;
            assert_eq!(
                rows,
                vec![
                    vec![Value::Integer(11), Value::Integer(expected)],
                    vec![Value::Integer(11), Value::Integer(expected)],
                    vec![Value::Null, Value::Null],
                    vec![Value::Integer(11), Value::Integer(expected)]
                ]
            );
            let prepared = c.prepare(
                "SELECT CAST({'t':$1}::VARIANT AS STRUCT(t INTEGER)),CAST($1 AS INTEGER)",
            )?;
            let result = c
                .execute_prepared(&prepared, &[Value::Varchar("parameter".into())])?
                .rows;
            let row = &result[0];
            assert_eq!(row[0].to_string(), format!("{{'t': {expected}}}"));
            assert_eq!(row[1], Value::Integer(11));
            c.execute("CREATE TABLE adapted(i INTEGER); INSERT INTO adapted SELECT CAST(v::VARIANT AS INTEGER) FROM (VALUES('a'),('b'),(NULL)) t(v)")?;
            assert_eq!(
                c.query("SELECT i FROM adapted ORDER BY i")?.rows,
                vec![
                    vec![Value::Integer(expected)],
                    vec![Value::Integer(expected)],
                    vec![Value::Null]
                ]
            );
            if aware {
                assert_eq!(
                    c.query("SELECT TRY_CAST(['a','invalid']::VARIANT AS INTEGER[])")?
                        .rows,
                    vec![vec![Value::Null]]
                );
                assert_eq!(
                    c.query("SELECT TRY_CAST(['a','invalid'] AS INTEGER[])::VARCHAR")?
                        .rows,
                    vec![vec![Value::Varchar("[11, 11]".into())]]
                );
                for input in ["resource", "internal", "malformed", "null"] {
                    let error = c
                        .query(&format!("SELECT TRY_CAST('{input}'::VARIANT AS INTEGER)"))
                        .unwrap_err();
                    if input == "resource" {
                        assert!(matches!(error, Error::Resource(_)));
                    } else {
                        assert!(matches!(error, Error::Internal(_)));
                    }
                }
                let before = c.query("SELECT * FROM adapted ORDER BY i")?.rows;
                assert!(
                    c.execute("UPDATE adapted SET i=CAST('invalid'::VARIANT AS INTEGER)")
                        .is_err()
                );
                assert_eq!(c.query("SELECT * FROM adapted ORDER BY i")?.rows, before);
            }
        }
    }
    Ok(())
}
