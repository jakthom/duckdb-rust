use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::{
        TypeParameter,
        cast::{CastMode, CastRegistry, CastSpec},
        type_registry::{
            TypeAdapter, TypeRegistry,
            ascii::{self, AsciiCast, MaterializedAscii, StreamingAscii},
        },
    },
    execution::{
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        operator::join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
    parallel::QueryContext,
    storage::{
        checkpoint::FileCheckpoint, duckdb::DuckDbFormat, filesystem::OpenMode,
        format::JsonSnapshotFormat,
    },
};
use std::{cmp::Ordering, sync::Arc};

#[path = "types/binding.rs"]
mod binding;
#[path = "types/cast_nulls.rs"]
mod cast_nulls;
#[path = "types/keys.rs"]
mod keys;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn composition(adapter: Arc<dyn TypeAdapter>) -> Result<(Arc<TypeRegistry>, CastRegistry)> {
    let mut types = TypeRegistry::builtins();
    types.register(ascii::FAMILY, adapter)?;
    let mut casts = CastRegistry::builtins();
    for width in [16, 64] {
        let data_type = ascii::data_type(width)?;
        casts.register_type(&data_type, &types)?;
        for mode in [CastMode::Explicit, CastMode::Assignment] {
            for (source, target) in [
                (DataType::Varchar, data_type.clone()),
                (data_type.clone(), DataType::Varchar),
            ] {
                casts.register(
                    CastSpec {
                        source,
                        target,
                        mode,
                    },
                    Arc::new(AsciiCast),
                )?;
            }
        }
    }
    Ok((Arc::new(types), casts))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn value(text: &str) -> Result<Value> {
    Ok(Value::extension(
        ascii::data_type(64)?,
        text.as_bytes().to_vec(),
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
#[cfg(target_pointer_width = "64")]
fn registered_metadata_preserves_the_primitive_value_footprint() {
    assert!(
        std::mem::size_of::<Value>() <= 32,
        "primitive Value footprint grew to {} bytes",
        std::mem::size_of::<Value>()
    );
    assert!(
        std::mem::size_of::<DataType>() <= 16,
        "logical type footprint grew to {} bytes",
        std::mem::size_of::<DataType>()
    );
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn registered_adapters_share_comparison_keys_parameters_and_ownership() -> Result<()> {
    let mut registries = Vec::new();
    for adapter in [
        Arc::new(MaterializedAscii) as Arc<dyn TypeAdapter>,
        Arc::new(StreamingAscii),
    ] {
        registries.push(composition(adapter)?.0);
    }
    let values = [
        "", "A", "a", "AA", "aA", "ab", "B", "b", "a\0b", "a\0B", "\0", "Z", "z", "[", "123",
    ];
    for registry in &registries {
        let q = QueryContext::background().with_types(registry.clone());
        let bound = registry.bind(&ascii::data_type(64)?)?;
        for a in values {
            for b in values {
                let expected = a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase());
                assert_eq!(bound.compare(&value(a)?, &value(b)?, &q)?, expected);
                let (mut a_key, mut b_key) = (vec![], vec![]);
                bound.append_key(&value(a)?, &mut a_key, &q)?;
                bound.append_key(&value(b)?, &mut b_key, &q)?;
                assert_eq!(a_key == b_key, expected == Ordering::Equal);
            }
        }
        let mut key = vec![9, 8, 7];
        assert!(bound.append_key(&value("é")?, &mut key, &q).is_err());
        assert_eq!(key, vec![9, 8, 7]);
        assert!(bound.validate(&value(&"x".repeat(65))?, &q).is_err());
        let wrong = Value::extension(ascii::data_type(16)?, b"text".to_vec());
        assert!(bound.validate(&wrong, &q).is_err());
        assert!(
            registry
                .bind(&DataType::extension(
                    ascii::FAMILY,
                    vec![TypeParameter::Integer(0)]
                ))
                .is_err()
        );
        assert!(
            registry
                .common_type(&ascii::data_type(16)?, &ascii::data_type(64)?)
                .is_err()
        );
    }
    let mut replacement = (*registries[0]).clone();
    let retained = replacement.bind(&ascii::data_type(64)?)?;
    replacement.replace(ascii::FAMILY, Arc::new(StreamingAscii))?;
    assert_eq!(retained.adapter(), "materialized-ascii-ci");
    assert_eq!(
        replacement.bind(&ascii::data_type(64)?)?.adapter(),
        "streaming-ascii-ci"
    );
    assert!(
        replacement
            .register(ascii::FAMILY, Arc::new(StreamingAscii))
            .is_err()
    );
    assert!(
        TypeRegistry::builtins()
            .bind(&ascii::data_type(64)?)
            .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn registered_values_work_through_sql_relational_operators_and_private_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut id = 0;
    for adapter in [
        Arc::new(MaterializedAscii) as Arc<dyn TypeAdapter>,
        Arc::new(StreamingAscii),
    ] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            for join in [
                Arc::new(HashJoin) as Arc<dyn JoinAlgorithm>,
                Arc::new(NestedLoopJoin),
            ] {
                id += 1;
                let (types, casts) = composition(adapter.clone())?;
                let path = directory.path().join(format!("registered{id}.db"));
                {
                    let db = DatabaseBuilder::new()
                        .types(types.clone())
                        .casts(casts.clone())
                        .indexes(indexes.clone())
                        .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![join])))
                        .durability(Arc::new(FileCheckpoint::open(
                            &path,
                            OpenMode::ReadWrite,
                            Arc::new(JsonSnapshotFormat),
                        )?))
                        .build()?;
                    let mut c = db.connect();
                    c.execute("CREATE TABLE items(k ascii_ci(64) PRIMARY KEY, n INTEGER, d ascii_ci(64) DEFAULT 'MiXeD'); INSERT INTO items(k,n) VALUES ('Alpha',1),('Beta',2); CREATE TABLE samples(k ascii_ci(64)); INSERT INTO samples VALUES ('ALPHA'),('alpha'),('beta'),(NULL)")?;
                    assert!(c.execute("INSERT INTO items(k) VALUES ('aLPHa')").is_err());
                    assert_eq!(
                        c.query(
                            "SELECT k::VARCHAR FROM items WHERE k=CAST('ALPHA' AS ascii_ci(64))"
                        )?
                        .rows,
                        vec![vec![Value::Varchar("Alpha".into())]]
                    );
                    assert_eq!(
                        c.query("SELECT count(*) FROM items JOIN samples ON items.k=samples.k")?
                            .rows,
                        vec![vec![Value::Integer(3)]]
                    );
                    assert_eq!(
                        c.query("SELECT n FROM items WHERE EXISTS(SELECT 1 FROM samples WHERE samples.k=items.k)")?.rows,
                        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
                    );
                    assert_eq!(
                        c.query("SELECT count(*) FROM samples WHERE NOT EXISTS(SELECT 1 FROM items WHERE items.k=samples.k)")?.rows,
                        vec![vec![Value::Integer(1)]]
                    );
                    assert_eq!(
                        c.query("SELECT count(DISTINCT k), count(*) FROM samples")?
                            .rows,
                        vec![vec![Value::Integer(2), Value::Integer(4)]]
                    );
                    assert_eq!(
                        c.query(
                            "SELECT min(k)::VARCHAR, count(*) FROM samples GROUP BY k ORDER BY k"
                        )?
                        .rows,
                        vec![
                            vec![Value::Varchar("ALPHA".into()), Value::Integer(2)],
                            vec![Value::Varchar("beta".into()), Value::Integer(1)],
                            vec![Value::Null, Value::Integer(1)]
                        ]
                    );
                    assert_eq!(c.query("SELECT count(*) FROM (SELECT k FROM samples UNION SELECT k FROM items) q")?.rows, vec![vec![Value::Integer(3)]]);
                    assert_eq!(c.query("SELECT nullif(CAST('A' AS ascii_ci(64)),CAST('a' AS ascii_ci(64))) IS NULL, TRY_CAST('é' AS ascii_ci(64)) IS NULL")?.rows, vec![vec![Value::Boolean(true), Value::Boolean(true)]]);
                    assert_eq!(
                        c.query("SELECT d FROM items LIMIT 1")?.columns[0].data_type,
                        ascii::data_type(64)?
                    );
                    c.execute("BEGIN; UPDATE items SET k='GAMMA' WHERE n=1; ROLLBACK; UPDATE items SET n=10 WHERE k=CAST('alpha' AS ascii_ci(64))")?;
                    let prepared = c.prepare("SELECT $1::VARCHAR")?;
                    assert_eq!(
                        c.execute_prepared(&prepared, &[value("Spelling")?])?.rows,
                        vec![vec![Value::Varchar("Spelling".into())]]
                    );
                    assert!(c.execute_prepared(&prepared, &[value("é")?]).is_err());
                }
                // Reopen with a different implementation of the same persisted identity.
                let (types, casts) = composition(Arc::new(StreamingAscii))?;
                let db = DatabaseBuilder::new()
                    .types(types)
                    .casts(casts)
                    .indexes(indexes.clone())
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        Arc::new(JsonSnapshotFormat),
                    )?))
                    .build()?;
                let mut c = db.connect();
                assert_eq!(
                    c.query(
                        "SELECT n,d::VARCHAR FROM items WHERE k=CAST('ALPHA' AS ascii_ci(64))"
                    )?
                    .rows,
                    vec![vec![Value::Integer(10), Value::Varchar("MiXeD".into())]]
                );
                assert!(c.execute("INSERT INTO items(k) VALUES ('BETA')").is_err());
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn missing_registration_and_unsupported_native_encoding_preserve_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("private.db");
    let (types, casts) = composition(Arc::new(StreamingAscii))?;
    {
        let db = DatabaseBuilder::new()
            .types(types.clone())
            .casts(casts.clone())
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()?;
        db.connect()
            .execute("CREATE TABLE t(k ascii_ci(64)); INSERT INTO t VALUES ('Kept')")?;
    }
    let bytes = std::fs::read(&path)?;
    assert!(matches!(
        DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat)
            )?))
            .build(),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(std::fs::read(&path)?, bytes);
    let native = directory.path().join("native.db");
    let db = DatabaseBuilder::new()
        .types(types)
        .casts(casts)
        .durability(Arc::new(FileCheckpoint::open(
            &native,
            OpenMode::ReadWrite,
            Arc::new(DuckDbFormat::default()),
        )?))
        .build()?;
    let mut c = db.connect();
    c.execute("CREATE TABLE kept(i INTEGER); INSERT INTO kept VALUES (42)")?;
    let bytes = std::fs::read(&native)?;
    assert!(matches!(
        c.execute("CREATE TABLE unsupported(k ascii_ci(64))"),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(std::fs::read(&native)?, bytes);
    assert!(c.query("SELECT * FROM unsupported").is_err());
    assert_eq!(
        c.query("SELECT * FROM kept")?.rows,
        vec![vec![Value::Integer(42)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn type_metadata_limits_and_retained_index_semantics_are_checked() -> Result<()> {
    let (types, _) = composition(Arc::new(StreamingAscii))?;
    let mut nested = DataType::Integer;
    for _ in 0..66 {
        nested = DataType::extension(ascii::FAMILY, vec![TypeParameter::Type(Box::new(nested))]);
    }
    assert!(matches!(types.bind(&nested), Err(Error::Resource(_))));
    assert!(matches!(
        CastRegistry::default().bind(&DataType::Null, &nested, CastMode::Explicit, &types),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        CastRegistry::default().register(
            CastSpec {
                source: DataType::Varchar,
                target: nested,
                mode: CastMode::Explicit
            },
            Arc::new(AsciiCast)
        ),
        Err(Error::Resource(_))
    ));
    assert_eq!(ascii::data_type(64)?.to_string(), "ascii_ci(64)");
    assert!(matches!(
        types.bind(&DataType::extension(
            ascii::FAMILY,
            vec![TypeParameter::Integer(1); 1025]
        )),
        Err(Error::Resource(_))
    ));
    assert!(
        types
            .bind(&DataType::extension("builtin.integer", vec![]))
            .is_err()
    );
    let context = QueryContext::background().with_types(types.clone());
    for factory in [
        Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
        Arc::new(BTreeIndexFactory),
    ] {
        let mut entries = vec![(7, vec![value("Alpha")?]), (9, vec![value("Beta")?])].into_iter();
        let index = factory.build(
            duckdb_rust::execution::index::IndexSpec {
                key_types: vec![ascii::data_type(64)?],
                unique: true,
            },
            &mut entries,
            &context,
        )?;
        // Lookup uses the index's retained semantics even when the resource
        // context does not contain this registered type.
        assert_eq!(
            index.lookup(&vec![value("ALPHA")?], &QueryContext::background())?,
            vec![7]
        );
    }
    let data_type = types.bind(&ascii::data_type(64)?)?;
    let mut first = vec![];
    let mut second = vec![];
    for text in ["a", "bc"] {
        data_type.append_key(&value(text)?, &mut first, &context)?;
    }
    for text in ["ab", "c"] {
        data_type.append_key(&value(text)?, &mut second, &context)?;
    }
    assert_ne!(first, second);
    let mut key = vec![7, 8, 9];
    let too_large = Value::Varchar("x".repeat(16 * 1024 * 1024));
    assert!(matches!(
        types
            .bind(&DataType::Varchar)?
            .append_key(&too_large, &mut key, &context),
        Err(Error::Resource(_))
    ));
    assert_eq!(key, vec![7, 8, 9]);
    Ok(())
}

#[derive(Debug)]
struct InvalidScalar;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::function::ScalarFunction for InvalidScalar {
    fn name(&self) -> &str {
        "invalid_registered_value"
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        ascii::data_type(64)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        value("é")
    }
}

#[derive(Debug)]
struct InvalidOperator(duckdb_rust::planner::Schema);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::execution::physical_plan::PhysicalOperator for InvalidOperator {
    fn schema(&self) -> &duckdb_rust::planner::Schema {
        &self.0
    }
    fn delivery(&self) -> duckdb_rust::execution::physical_plan::DeliveryMode {
        duckdb_rust::execution::physical_plan::DeliveryMode::Incremental
    }
    fn open<'a>(
        &'a self,
        _: &'a duckdb_rust::execution::ExecutionContext<'a>,
    ) -> Result<duckdb_rust::execution::stream::Stream<'a>> {
        struct Batch(Option<duckdb_rust::common::vector::DataChunk>);
        impl duckdb_rust::execution::stream::BatchStream for Batch {
            fn next(&mut self, _: usize) -> Result<Option<duckdb_rust::common::vector::DataChunk>> {
                Ok(self.0.take())
            }
        }
        let types: Vec<_> = self.0.iter().map(|field| field.data_type.clone()).collect();
        let row = types
            .iter()
            .map(|data_type| match data_type {
                DataType::Extension(_) => {
                    Value::extension(data_type.clone(), "é".as_bytes().to_vec())
                }
                _ => Value::Null,
            })
            .collect();
        let chunk = duckdb_rust::common::vector::DataChunk::from_rows(&types, &[row])?;
        Ok(Box::new(Batch(Some(chunk))))
    }
}

struct InvalidPlanner;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::execution::physical_plan::PhysicalPlanner for InvalidPlanner {
    fn name(&self) -> &'static str {
        "invalid-registered-operator"
    }
    fn plan(
        &self,
        plan: &duckdb_rust::planner::LogicalPlan,
    ) -> Result<Arc<dyn duckdb_rust::execution::physical_plan::PhysicalOperator>> {
        Ok(Arc::new(InvalidOperator(plan.schema.clone())))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn malformed_function_and_operator_values_never_escape_or_become_try_cast_nulls() -> Result<()> {
    let (types, casts) = composition(Arc::new(StreamingAscii))?;
    let mut functions = duckdb_rust::function::FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(InvalidScalar))?;
    let mut c = DatabaseBuilder::new()
        .types(types.clone())
        .casts(casts.clone())
        .functions(functions)
        .build()?
        .connect();
    assert!(matches!(
        c.query("SELECT TRY_CAST(invalid_registered_value() AS VARCHAR)"),
        Err(Error::Internal(_))
    ));
    let mut c = DatabaseBuilder::new()
        .types(types)
        .casts(casts)
        .physical_planner(Arc::new(InvalidPlanner))
        .build()?
        .connect();
    for sql in [
        "SELECT CAST('valid' AS ascii_ci(64))",
        "SELECT 1, CAST('valid' AS ascii_ci(64)), false",
        "SELECT 1, false, CAST('valid' AS ascii_ci(64))",
        "SELECT CAST('valid' AS ascii_ci(64)), 1, CAST('valid' AS ascii_ci(16))",
    ] {
        assert!(matches!(c.query(sql), Err(Error::Internal(_))), "{sql}");
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_boolean_integral_combination_does_not_widen_implicit_casts() -> Result<()> {
    use duckdb_rust::common::NestedType;
    let types = TypeRegistry::builtins();
    let casts = CastRegistry::builtins();
    for integer in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
        DataType::UTinyInt,
        DataType::USmallInt,
        DataType::UInteger,
        DataType::UBigInt,
        DataType::UHugeInt,
    ] {
        for (left, right) in [
            (DataType::Boolean, integer.clone()),
            (integer.clone(), DataType::Boolean),
        ] {
            assert_eq!(types.common_type(&left, &right)?, integer);
            let nested = |ty| {
                NestedType::List(NestedType::Struct(vec![("n".into(), ty)]).data_type()).data_type()
            };
            assert_eq!(
                types.common_type(&nested(left), &nested(right))?,
                nested(integer.clone())
            );
        }
        assert!(
            casts
                .bind(&DataType::Boolean, &integer, CastMode::Implicit, &types)
                .is_err()
        );
        assert!(
            casts
                .bind(&DataType::Boolean, &integer, CastMode::Explicit, &types)
                .is_ok()
        );
    }
    for other in [
        DataType::Float,
        DataType::Double,
        DataType::Decimal { width: 8, scale: 2 },
    ] {
        assert!(types.common_type(&DataType::Boolean, &other).is_err());
        assert!(types.common_type(&other, &DataType::Boolean).is_err());
    }
    Ok(())
}
