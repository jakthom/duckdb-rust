use super::*;
use duckdb_rust::common::type_registry::{KeyWriter, PrimitiveTypes};
use std::sync::Mutex;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integer_literal_common_types_preserve_no_hint_rules_and_checked_boundaries() -> Result<()> {
    let types = TypeRegistry::builtins();
    let targets = [
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
    ];
    for target in targets {
        for number in [
            i128::MIN,
            i64::MIN as i128,
            -32769,
            -32768,
            -129,
            -128,
            -1,
            0,
            127,
            128,
            255,
            256,
            32767,
            32768,
            65535,
            65536,
            i32::MAX as i128,
            i32::MAX as i128 + 1,
            i64::MAX as i128,
            i64::MAX as i128 + 1,
            u64::MAX as i128,
            i128::MAX,
        ] {
            let fits = if target.is_unsigned_integer() {
                number >= 0 && Value::Unsigned(number as u128).fits_type(&target)
            } else {
                Value::Integer(number).fits_type(&target)
            };
            let source = DataType::HugeInt;
            let ordinary = types.try_common_type(&source, &target)?;
            let expected = if fits {
                Some(target.clone())
            } else {
                ordinary.clone()
            };
            assert_eq!(
                types.try_common_type_with_integer_literals(
                    &source,
                    &target,
                    Some(number),
                    None
                )?,
                expected,
                "{number}/{target}"
            );
            assert_eq!(
                types.try_common_type_with_integer_literals(
                    &target,
                    &source,
                    None,
                    Some(number)
                )?,
                expected,
                "reversed {number}/{target}"
            );
            assert_eq!(
                types.try_common_type_with_integer_literals(&source, &target, None, None)?,
                ordinary
            );
        }
    }
    for (left, right) in [(1, 1), (1, 2), (i128::MAX, 1)] {
        assert_eq!(
            types.try_common_type_with_integer_literals(
                &DataType::HugeInt,
                &DataType::Integer,
                Some(left),
                Some(right)
            )?,
            Some(DataType::HugeInt)
        );
    }
    for target in [
        DataType::Boolean,
        DataType::Double,
        DataType::Decimal { width: 5, scale: 2 },
        DataType::Null,
    ] {
        assert_eq!(
            types.try_common_type_with_integer_literals(
                &DataType::Integer,
                &target,
                Some(1),
                None
            )?,
            types.try_common_type(&DataType::Integer, &target)?
        );
    }
    for (ty, value) in [
        (DataType::TinyInt, 128),
        (DataType::UTinyInt, 1),
        (DataType::Varchar, 1),
        (DataType::Null, 1),
    ] {
        assert!(matches!(
            types.try_common_type_with_integer_literals(&ty, &DataType::Integer, Some(value), None),
            Err(Error::Bind(_))
        ));
    }
    Ok(())
}

type Call = (DataType, DataType, Option<i128>, Option<i128>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn case_and_collection_inference_retain_selected_integer_literal_proposals() -> Result<()> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut types = TypeRegistry::builtins();
    for family in ["builtin.integer", "builtin.tinyint"] {
        types.replace(
            family,
            Arc::new(Hinted {
                target: DataType::SmallInt,
                calls: calls.clone(),
            }),
        )?;
    }
    let mut c = DatabaseBuilder::new()
        .types(Arc::new(types))
        .build()?
        .connect();
    for sql in [
        "SELECT typeof(CASE WHEN false THEN 2::TINYINT ELSE 7 END)",
        "SELECT typeof([7,2::TINYINT])",
    ] {
        calls.lock().unwrap().clear();
        let expected = if sql.contains('[') {
            "SMALLINT[]"
        } else {
            "SMALLINT"
        };
        assert_eq!(
            c.query(sql)?.rows,
            vec![vec![Value::Varchar(expected.into())]]
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (DataType::Integer, DataType::TinyInt, Some(7), None),
                (DataType::TinyInt, DataType::Integer, None, Some(7))
            ]
        );
    }
    Ok(())
}
#[derive(Debug)]
struct Hinted {
    target: DataType,
    calls: Arc<Mutex<Vec<Call>>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for Hinted {
    fn name(&self) -> &'static str {
        "selected-literal-common-type"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, q: &QueryContext) -> Result<()> {
        PrimitiveTypes.validate_value(ty, value, q)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn common_type_with_integer_literals(
        &self,
        a: &DataType,
        b: &DataType,
        av: Option<i128>,
        bv: Option<i128>,
        _: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        self.calls
            .lock()
            .unwrap()
            .push((a.clone(), b.clone(), av, bv));
        Ok(Some(self.target.clone()))
    }
    fn compare(&self, ty: &DataType, a: &Value, b: &Value, q: &QueryContext) -> Result<Ordering> {
        PrimitiveTypes.compare(ty, a, b, q)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        out: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(ty, value, out, q)
    }
}

#[derive(Debug)]
struct Legacy;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for Legacy {
    fn name(&self) -> &'static str {
        "legacy-common-type-without-literal-hook"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, q: &QueryContext) -> Result<()> {
        PrimitiveTypes.validate_value(ty, value, q)
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Ok(None)
    }
    fn common_type_with_registry(
        &self,
        _: &DataType,
        _: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        types.bind(&DataType::BigInt)?;
        Ok(Some(DataType::BigInt))
    }
    fn compare(&self, ty: &DataType, a: &Value, b: &Value, q: &QueryContext) -> Result<Ordering> {
        PrimitiveTypes.compare(ty, a, b, q)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        out: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(ty, value, out, q)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integer_literal_inference_retains_selected_defaults_operand_roles_and_conflicts() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    let calls = Arc::new(Mutex::new(Vec::new()));
    for family in ["builtin.integer", "builtin.tinyint"] {
        types.replace(
            family,
            Arc::new(Hinted {
                target: DataType::SmallInt,
                calls: calls.clone(),
            }),
        )?;
    }
    assert_eq!(
        types.try_common_type_with_integer_literals(
            &DataType::Integer,
            &DataType::TinyInt,
            Some(7),
            None
        )?,
        Some(DataType::SmallInt)
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            (DataType::Integer, DataType::TinyInt, Some(7), None),
            (DataType::TinyInt, DataType::Integer, None, Some(7))
        ]
    );
    calls.lock().unwrap().clear();
    assert_eq!(
        types.try_common_type_with_integer_literals(
            &DataType::Integer,
            &DataType::Integer,
            Some(7),
            None
        )?,
        Some(DataType::SmallInt)
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    calls.lock().unwrap().clear();
    assert_eq!(
        types.try_common_type(&DataType::Integer, &DataType::TinyInt)?,
        Some(DataType::Integer)
    );
    assert_eq!(
        types.try_common_type_with_integer_literals(
            &DataType::Integer,
            &DataType::TinyInt,
            None,
            None
        )?,
        Some(DataType::Integer)
    );
    assert!(calls.lock().unwrap().is_empty());
    types.replace(
        "builtin.tinyint",
        Arc::new(Hinted {
            target: DataType::HugeInt,
            calls: calls.clone(),
        }),
    )?;
    assert!(
        matches!(types.try_common_type_with_integer_literals(&DataType::Integer,&DataType::TinyInt,Some(7),None),Err(Error::Bind(message)) if message.contains("disagree"))
    );
    for family in ["builtin.integer", "builtin.tinyint"] {
        types.replace(family, Arc::new(Legacy))?;
    }
    assert_eq!(
        types.try_common_type_with_integer_literals(
            &DataType::Integer,
            &DataType::TinyInt,
            Some(7),
            None
        )?,
        Some(DataType::BigInt)
    );
    assert_eq!(
        types.try_common_type(&DataType::Integer, &DataType::TinyInt)?,
        Some(DataType::BigInt)
    );
    // Selected proposed metadata still passes registration and shape validation.
    types.replace(
        "builtin.integer",
        Arc::new(Hinted {
            target: DataType::Decimal { width: 0, scale: 0 },
            calls,
        }),
    )?;
    assert!(matches!(
        types.try_common_type_with_integer_literals(
            &DataType::Integer,
            &DataType::Integer,
            Some(1),
            None
        ),
        Err(Error::Bind(_))
    ));
    Ok(())
}
