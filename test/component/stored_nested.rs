//! Constructor children stay as expressions until the selected evaluator runs.
use super::*;
use duckdb_rust::{
    common::{NestedPayload, NestedType},
    function::ScalarBindArguments,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn named(
    name: &str,
    fields: Vec<(&str, StoredExpression)>,
    style: StoredArgumentStyle,
) -> StoredExpression {
    StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Function {
            name: vec![name.into()],
            arguments: fields
                .into_iter()
                .map(|(name, expression)| StoredArgument {
                    name: Some(name.into()),
                    expression,
                })
                .collect(),
            is_operator: false,
            argument_style: style,
        },
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn operator(kind: StoredOperator, children: Vec<StoredExpression>) -> StoredExpression {
    StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Operator { kind, children },
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stored_nested_constructors_keep_typed_nulls_names_and_selected_expression_children() -> Result<()>
{
    let query = QueryContext::background();
    let casts = CastRegistry::builtins();
    let functions = FunctionRegistry::builtins();
    let qualified_list = StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Function {
            name: vec!["main".into(), "list_value".into()],
            arguments: vec![StoredArgument {
                name: None,
                expression: StoredExpression::literal(DataType::Integer, Value::Integer(1)),
            }],
            is_operator: false,
            argument_style: StoredArgumentStyle::Named,
        },
    };
    assert_eq!(
        bind(
            &qualified_list,
            &SqlBinder,
            &functions,
            &casts,
            &ScalarEvaluator,
            &query,
        )?
        .data_type,
        NestedType::List(DataType::Integer).data_type()
    );
    let decimal = DataType::Decimal {
        width: 12,
        scale: 2,
    };
    for style in [
        StoredArgumentStyle::Named,
        StoredArgumentStyle::LegacyAliases,
    ] {
        let child = named(
            "struct_pack",
            vec![
                (
                    "Amount",
                    StoredExpression::literal(decimal.clone(), Value::Null),
                ),
                (
                    "Count",
                    StoredExpression::literal(DataType::UTinyInt, Value::Unsigned(7)),
                ),
            ],
            style,
        );
        let structure = NestedType::Struct(vec![
            ("Amount".into(), decimal.clone()),
            ("Count".into(), DataType::UTinyInt),
        ])
        .data_type();
        let sequence = operator(
            StoredOperator::ListConstructor,
            vec![
                child.clone(),
                cast(
                    StoredExpression::literal(DataType::Null, Value::Null),
                    structure.clone(),
                    false,
                ),
            ],
        );
        let expressions = [
            (child.clone(), structure.clone()),
            (
                sequence.clone(),
                NestedType::List(structure.clone()).data_type(),
            ),
            (
                cast(
                    sequence.clone(),
                    NestedType::Array {
                        element: structure.clone(),
                        length: 2,
                    }
                    .data_type(),
                    false,
                ),
                NestedType::Array {
                    element: structure.clone(),
                    length: 2,
                }
                .data_type(),
            ),
            (
                call("row", vec![child.clone()]),
                NestedType::Tuple(vec![structure.clone()]).data_type(),
            ),
            (
                call(
                    "map",
                    vec![
                        call(
                            "list_value",
                            vec![StoredExpression::literal(
                                DataType::Varchar,
                                Value::Varchar("k".into()),
                            )],
                        ),
                        call("list_value", vec![child.clone()]),
                    ],
                ),
                NestedType::Map {
                    key: DataType::Varchar,
                    value: structure.clone(),
                }
                .data_type(),
            ),
            (
                named(
                    "union_value",
                    vec![(
                        "present",
                        StoredExpression::literal(decimal.clone(), Value::Null),
                    )],
                    style,
                ),
                NestedType::Union(vec![("present".into(), decimal.clone())]).data_type(),
            ),
            (
                cast(child.clone(), NestedType::Variant.data_type(), false),
                NestedType::Variant.data_type(),
            ),
        ];
        for evaluator in [
            &ScalarEvaluator as &dyn ExpressionEvaluator,
            &BatchedEvaluator,
        ] {
            for (expression, ty) in &expressions {
                let bound = bind(
                    expression, &SqlBinder, &functions, &casts, evaluator, &query,
                )?;
                assert_eq!(&bound.data_type, ty);
                let value = evaluator.evaluate(&bound, &vec![], &query)?;
                assert!(!value.is_null());
                query.types().bind(ty)?.validate(&value, &query)?;
            }
            let extracted = operator(
                StoredOperator::Field,
                vec![
                    operator(
                        StoredOperator::Index,
                        vec![
                            sequence.clone(),
                            StoredExpression::literal(DataType::Integer, Value::Integer(1)),
                        ],
                    ),
                    StoredExpression::literal(DataType::Varchar, Value::Varchar("amount".into())),
                ],
            );
            let bound = bind(
                &extracted, &SqlBinder, &functions, &casts, evaluator, &query,
            )?;
            assert_eq!(bound.data_type, decimal);
            assert_eq!(evaluator.evaluate(&bound, &vec![], &query)?, Value::Null);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct NamedProbe {
    name: &'static str,
    accepts: bool,
    expected_name: Option<&'static str>,
    expected_alias: Option<&'static str>,
    bound: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for NamedProbe {
    fn name(&self) -> &str {
        self.name
    }
    fn accepts_named_arguments(&self) -> bool {
        self.accepts
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        assert_eq!(arguments.len(), 1);
        assert_eq!(arguments.argument_name(0)?, self.expected_name);
        assert_eq!(arguments.argument_alias(0)?, self.expected_alias);
        assert_eq!(arguments.data_type(0)?, DataType::SmallInt);
        assert!(arguments.argument_name(1).is_err());
        assert!(arguments.argument_alias(1).is_err());
        Ok(Some(Arc::new(Self {
            bound: true,
            ..*self
        })))
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Integer)
    }
    fn evaluate(&self, arguments: &[Value], _: &QueryContext) -> Result<Value> {
        assert!(self.bound);
        assert_eq!(arguments, &[Value::Null]);
        Ok(Value::Integer(41))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ordinary_and_stored_named_calls_use_selected_adapters_and_distinguish_legacy_aliases()
-> Result<()> {
    for name in ["struct_pack", "union_value", "replacement"] {
        for accepts in [false, true] {
            let mut functions = FunctionRegistry::default();
            functions.register_scalar(Arc::new(NamedProbe {
                name,
                accepts,
                expected_name: Some("Kept"),
                expected_alias: Some("Kept"),
                bound: false,
            }))?;
            let mut connection = DatabaseBuilder::new()
                .functions(functions.clone())
                .build()?
                .connect();
            let sql = format!("SELECT {name}(Kept := NULL::SMALLINT)");
            let expression = named(
                name,
                vec![(
                    "Kept",
                    StoredExpression::literal(DataType::SmallInt, Value::Null),
                )],
                StoredArgumentStyle::Named,
            );
            let query = QueryContext::background();
            let bound = bind(
                &expression,
                &SqlBinder,
                &functions,
                &CastRegistry::builtins(),
                &ScalarEvaluator,
                &query,
            );
            if accepts {
                assert_eq!(connection.query(&sql)?.rows, vec![vec![Value::Integer(41)]]);
                assert_eq!(
                    ScalarEvaluator.evaluate(&bound?, &vec![], &query)?,
                    Value::Integer(41)
                );
            } else {
                assert!(matches!(connection.query(&sql), Err(Error::Bind(_))));
                assert!(matches!(bound, Err(Error::Bind(_))));
            }
        }
    }
    let mut functions = FunctionRegistry::default();
    functions.register_scalar(Arc::new(NamedProbe {
        name: "replacement",
        accepts: false,
        expected_name: None,
        expected_alias: Some("Legacy"),
        bound: false,
    }))?;
    let expression = named(
        "replacement",
        vec![(
            "Legacy",
            StoredExpression::literal(DataType::SmallInt, Value::Null),
        )],
        StoredArgumentStyle::LegacyAliases,
    );
    let query = QueryContext::background();
    let bound = bind(
        &expression,
        &SqlBinder,
        &functions,
        &CastRegistry::default(),
        &ScalarEvaluator,
        &query,
    )?;
    assert_eq!(
        ScalarEvaluator.evaluate(&bound, &vec![], &query)?,
        Value::Integer(41)
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stored_constructor_binding_does_not_evaluate_children_or_erase_null_union_tags() -> Result<()> {
    let query = QueryContext::background();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(SelectedFunction(17, false, calls.clone())))?;
    let casts = CastRegistry::builtins();
    let expression = named(
        "struct_pack",
        vec![("child", call("stored_function", vec![]))],
        StoredArgumentStyle::Named,
    );
    let bound = bind(
        &expression,
        &SqlBinder,
        &functions,
        &casts,
        &ScalarEvaluator,
        &query,
    )?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = ScalarEvaluator.evaluate(&bound, &vec![], &query)?;
    assert!(!result.is_null());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let bad = cast(
        StoredExpression::literal(DataType::Varchar, Value::Varchar("bad".into())),
        DataType::Integer,
        false,
    );
    let expression = named(
        "struct_pack",
        vec![("bad", bad)],
        StoredArgumentStyle::Named,
    );
    let bound = bind(
        &expression,
        &SqlBinder,
        &functions,
        &casts,
        &ScalarEvaluator,
        &query,
    )?;
    assert!(matches!(
        ScalarEvaluator.evaluate(&bound, &vec![], &query),
        Err(Error::Conversion(_))
    ));
    let expression = named(
        "union_value",
        vec![(
            "present",
            StoredExpression::literal(DataType::Integer, Value::Null),
        )],
        StoredArgumentStyle::Named,
    );
    let bound = bind(
        &expression,
        &SqlBinder,
        &functions,
        &casts,
        &ScalarEvaluator,
        &query,
    )?;
    let Value::Nested(result) = ScalarEvaluator.evaluate(&bound, &vec![], &query)? else {
        panic!("active NULL member is non-NULL UNION")
    };
    assert!(matches!(
        result.payload,
        NestedPayload::Union {
            tag: 0,
            value: Value::Null
        }
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn literal_syntax_selects_constructor_catalog_and_explicit_calls_share_collection_inference()
-> Result<()> {
    for (name, sql, named) in [
        ("list_value", "SELECT [NULL::SMALLINT]", false),
        ("list_value", "SELECT ARRAY[NULL::SMALLINT]", false),
        ("struct_pack", "SELECT {'Kept':NULL::SMALLINT}", true),
    ] {
        let mut connection = DatabaseBuilder::new()
            .functions(FunctionRegistry::default())
            .build()?
            .connect();
        assert!(
            matches!(connection.query(sql), Err(Error::Catalog(message)) if message.contains(name))
        );
        let mut functions = FunctionRegistry::default();
        functions.register_scalar(Arc::new(NamedProbe {
            name,
            accepts: named,
            expected_name: named.then_some("Kept"),
            expected_alias: named.then_some("Kept"),
            bound: false,
        }))?;
        let mut connection = DatabaseBuilder::new()
            .functions(functions)
            .build()?
            .connect();
        assert_eq!(connection.query(sql)?.rows, vec![vec![Value::Integer(41)]]);
    }
    let mut connection = Database::memory()?.connect();
    for (arguments, expected) in [
        ("1,1,NULL,3::TINYINT", "TINYINT"),
        ("1,2,3::TINYINT", "INTEGER"),
        ("NULL,1,3::TINYINT", "INTEGER"),
        ("'1',NULL,'2',3", "INTEGER"),
        ("true,1,NULL", "INTEGER"),
        ("1::UTINYINT,2::TINYINT", "SMALLINT"),
        ("NULL::DECIMAL(12,2),1.25", "DECIMAL(12,2)"),
    ] {
        assert_eq!(
            connection
                .query(&format!(
                    "SELECT typeof(list_value({arguments})),typeof([{arguments}])"
                ))?
                .rows,
            vec![vec![Value::Varchar(format!("{expected}[]")); 2]]
        );
    }
    assert_eq!(connection.query("SELECT typeof(array_value(1,2)),typeof(ARRAY[1,2]),typeof(struct_pack()),typeof(row())")?.rows,
        vec![vec![Value::Varchar("INTEGER[2]".into()), Value::Varchar("INTEGER[]".into()), Value::Varchar("STRUCT".into()), Value::Varchar("TUPLE".into())]]);
    for sql in [
        "SELECT list_value(NULL,'1',2)",
        "SELECT list_value('1'::VARCHAR,2)",
        "SELECT array_value()",
        "SELECT struct_pack(a:=1,A:=2)",
        "SELECT union_value(a:=1,b:=2)",
        "SELECT struct_pack(1)",
        "SELECT union_value(1)",
        "SELECT struct_pack(a:=1,2)",
    ] {
        assert!(connection.query(sql).is_err(), "{sql}");
    }
    let parameter = connection.prepare("SELECT list_value($1,1)")?;
    assert!(
        connection
            .execute_prepared(&parameter, &[Value::Varchar("2".into())])
            .is_err()
    );
    assert_eq!(
        connection
            .query("SELECT struct_pack(a:=1,b)::VARCHAR FROM (VALUES (2))t(b)")?
            .rows,
        vec![vec![Value::Varchar("{'a': 1, 'b': 2}".into())]]
    );
    let mut expression = named(
        "struct_pack",
        vec![(
            "a",
            StoredExpression::literal(DataType::Integer, Value::Integer(1)),
        )],
        StoredArgumentStyle::Named,
    );
    let mut child = StoredExpression::literal(DataType::SmallInt, Value::Null);
    child.alias = Some("b".into());
    let StoredExpressionKind::Function { arguments, .. } = &mut expression.kind else {
        unreachable!()
    };
    arguments.push(StoredArgument {
        name: None,
        expression: child,
    });
    let query = QueryContext::background();
    let bound = bind(
        &expression,
        &SqlBinder,
        &FunctionRegistry::builtins(),
        &CastRegistry::builtins(),
        &ScalarEvaluator,
        &query,
    )?;
    assert_eq!(
        bound.data_type,
        NestedType::Struct(vec![
            ("a".into(), DataType::Integer),
            ("b".into(), DataType::SmallInt)
        ])
        .data_type()
    );
    Ok(())
}

#[derive(Debug)]
struct RetainedChild(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::common::type_registry::TypeAdapter for RetainedChild {
    fn name(&self) -> &'static str {
        "retained-constructor-child"
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        duckdb_rust::common::type_registry::PrimitiveTypes.validate_type(data_type)
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        duckdb_rust::common::type_registry::PrimitiveTypes.common_type(left, right)
    }
    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        query: &QueryContext,
    ) -> Result<()> {
        if self.0 {
            return Err(Error::Resource("retained constructor child failure".into()));
        }
        duckdb_rust::common::type_registry::PrimitiveTypes.validate_value(data_type, value, query)
    }
    fn compare(
        &self,
        ty: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        duckdb_rust::common::type_registry::PrimitiveTypes.compare(ty, left, right, query)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        output: &mut duckdb_rust::common::type_registry::KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        duckdb_rust::common::type_registry::PrimitiveTypes.write_key(ty, value, output, query)
    }
}

struct ConstructorArguments;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for ConstructorArguments {
    fn len(&self) -> usize {
        1
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        if index == 0 {
            Ok(DataType::Integer)
        } else {
            Err(Error::Bind("argument bounds".into()))
        }
    }
    fn argument_alias(&self, index: usize) -> Result<Option<&str>> {
        self.data_type(index).map(|_| Some("Kept"))
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("constructors must not request values")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bound_named_constructors_retain_selected_children_without_ambient_registry_and_keep_failures_fatal()
-> Result<()> {
    let functions = FunctionRegistry::builtins();
    let ambient = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    for name in ["struct_pack", "union_value", "row"] {
        for failure in [false, true] {
            let mut types = TypeRegistry::builtins();
            types.replace("builtin.integer", Arc::new(RetainedChild(failure)))?;
            let query = QueryContext::background().with_types(Arc::new(types));
            let bound = functions
                .scalar(name)?
                .bind(&ConstructorArguments, &query)?
                .expect("selected constructor");
            drop(query);
            let result = bound.evaluate(&[Value::Integer(1)], &ambient);
            if failure {
                assert!(
                    matches!(result, Err(Error::Resource(message)) if message=="retained constructor child failure")
                );
            } else {
                assert!(!result?.is_null());
            }
            assert!(matches!(
                bound.evaluate(&[], &ambient),
                Err(Error::Internal(_))
            ));
            let interrupt = InterruptHandle::default();
            let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
            interrupt.interrupt();
            assert!(matches!(
                bound.evaluate(&[Value::Integer(1)], &cancelled),
                Err(Error::Interrupted)
            ));
        }
    }
    assert!(matches!(
        functions
            .scalar("list_value")?
            .bind(&ConstructorArguments, &ambient),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}
