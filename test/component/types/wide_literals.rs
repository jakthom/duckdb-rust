use super::*;
use duckdb_rust::common::type_registry::{IntegerLiteral, KeyWriter};
use std::sync::Mutex;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn builtin_wide_literal_proposals_keep_exact_domains_and_pairwise_normalization() -> Result<()> {
    let types = TypeRegistry::builtins();
    for target in [
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
        let ordinary = types.try_common_type(&DataType::UHugeInt, &target)?;
        for value in [
            0,
            1,
            127,
            128,
            255,
            256,
            u32::MAX as u128,
            u64::MAX as u128,
            i128::MAX as u128,
            (i128::MAX as u128) + 1,
            u128::MAX,
        ] {
            let fits = if target.is_unsigned_integer() {
                Value::Unsigned(value).fits_type(&target)
            } else {
                i128::try_from(value).is_ok_and(|v| Value::Integer(v).fits_type(&target))
            };
            let expected = if fits {
                Some(target.clone())
            } else {
                ordinary.clone()
            };
            let hint = Some(IntegerLiteral::Unsigned(value));
            assert_eq!(
                types.try_common_type_with_literals(&DataType::UHugeInt, &target, hint, None)?,
                expected
            );
            assert_eq!(
                types.try_common_type_with_literals(&target, &DataType::UHugeInt, None, hint)?,
                expected
            );
        }
    }
    let hint = Some(IntegerLiteral::Unsigned(u128::MAX));
    assert_eq!(
        types.try_common_type_with_literals(
            &DataType::UHugeInt,
            &DataType::Integer,
            hint,
            Some(IntegerLiteral::Signed(1))
        )?,
        Some(DataType::BigInt)
    );
    assert_eq!(
        types.try_common_type_with_literals(
            &DataType::UHugeInt,
            &DataType::Integer,
            None,
            Some(IntegerLiteral::Signed(1))
        )?,
        Some(DataType::UHugeInt)
    );
    assert_eq!(
        types.try_common_type_with_literals(
            &DataType::UHugeInt,
            &DataType::UHugeInt,
            hint,
            Some(IntegerLiteral::Unsigned(1))
        )?,
        Some(DataType::UHugeInt)
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Ordinary(DataType, DataType),
    Signed(DataType, DataType, Option<i128>, Option<i128>),
    Full(
        DataType,
        DataType,
        Option<IntegerLiteral>,
        Option<IntegerLiteral>,
    ),
}

#[derive(Debug)]
struct Legacy {
    calls: Arc<Mutex<Vec<Call>>>,
    target: DataType,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for Legacy {
    fn name(&self) -> &'static str {
        "legacy-signed-literal-contract"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        TypeRegistry::builtins().bind(ty).map(|_| ())
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        TypeRegistry::builtins().bind(ty)?.validate(value, query)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Ordinary(a.clone(), b.clone()));
        Ok(Some(self.target.clone()))
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
            .push(Call::Signed(a.clone(), b.clone(), av, bv));
        Ok(Some(self.target.clone()))
    }
    fn compare(&self, ty: &DataType, a: &Value, b: &Value, q: &QueryContext) -> Result<Ordering> {
        TypeRegistry::builtins().bind(ty)?.compare(a, b, q)
    }
    fn write_key(
        &self,
        ty: &DataType,
        v: &Value,
        out: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        duckdb_rust::common::type_registry::PrimitiveTypes.write_key(ty, v, out, q)
    }
}

#[derive(Debug)]
struct Full(Legacy, bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for Full {
    fn name(&self) -> &'static str {
        "full-width-literal-contract"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        self.0.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, v: &Value, q: &QueryContext) -> Result<()> {
        self.0.validate_value(ty, v, q)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        self.0.common_type(a, b)
    }
    fn common_type_with_literals(
        &self,
        a: &DataType,
        b: &DataType,
        av: Option<IntegerLiteral>,
        bv: Option<IntegerLiteral>,
        _: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        self.0
            .calls
            .lock()
            .unwrap()
            .push(Call::Full(a.clone(), b.clone(), av, bv));
        if self.1 {
            return Err(Error::Resource("selected literal inference failure".into()));
        }
        Ok(Some(self.0.target.clone()))
    }
    fn compare(&self, ty: &DataType, a: &Value, b: &Value, q: &QueryContext) -> Result<Ordering> {
        self.0.compare(ty, a, b, q)
    }
    fn write_key(
        &self,
        ty: &DataType,
        v: &Value,
        out: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        self.0.write_key(ty, v, out, q)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn full_literal_interface_preserves_unsigned_payloads_operand_roles_and_conflicts() -> Result<()> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut types = TypeRegistry::builtins();
    for family in ["builtin.hugeint", "builtin.uhugeint"] {
        types.replace(
            family,
            Arc::new(Full(
                Legacy {
                    calls: calls.clone(),
                    target: DataType::Double,
                },
                false,
            )),
        )?;
    }
    let a = Some(IntegerLiteral::Unsigned(u128::MAX));
    let b = Some(IntegerLiteral::Signed(i128::MIN));
    assert_eq!(
        types.try_common_type_with_literals(&DataType::UHugeInt, &DataType::HugeInt, a, b)?,
        Some(DataType::Double)
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            Call::Full(DataType::UHugeInt, DataType::HugeInt, a, b),
            Call::Full(DataType::HugeInt, DataType::UHugeInt, b, a)
        ]
    );
    calls.lock().unwrap().clear();
    let other = Some(IntegerLiteral::Unsigned(1));
    assert_eq!(
        types.try_common_type_with_literals(&DataType::UHugeInt, &DataType::UHugeInt, a, other)?,
        Some(DataType::Double)
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![Call::Full(DataType::UHugeInt, DataType::UHugeInt, a, other)]
    );
    for (target, fatal) in [
        (DataType::Integer, false),
        (DataType::Decimal { width: 0, scale: 0 }, false),
        (DataType::Double, true),
    ] {
        types.replace(
            "builtin.uhugeint",
            Arc::new(Full(
                Legacy {
                    calls: calls.clone(),
                    target,
                },
                fatal,
            )),
        )?;
        let error = types
            .try_common_type_with_literals(&DataType::UHugeInt, &DataType::HugeInt, a, b)
            .unwrap_err();
        assert!(if fatal {
            matches!(error, Error::Resource(_))
        } else {
            matches!(error, Error::Bind(_))
        });
    }
    types.replace(
        "builtin.uhugeint",
        Arc::new(Full(
            Legacy {
                calls,
                target: DataType::Decimal { width: 0, scale: 0 },
            },
            false,
        )),
    )?;
    assert!(matches!(
        types.try_common_type_with_literals(&DataType::UHugeInt, &DataType::UHugeInt, a, other),
        Err(Error::Bind(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn signed_compatibility_never_forwards_a_partial_or_narrowed_unsigned_hint() -> Result<()> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut types = TypeRegistry::builtins();
    for family in ["builtin.hugeint", "builtin.uhugeint"] {
        types.replace(
            family,
            Arc::new(Legacy {
                calls: calls.clone(),
                target: DataType::Double,
            }),
        )?;
    }
    assert_eq!(
        types.try_common_type_with_integer_literals(
            &DataType::HugeInt,
            &DataType::HugeInt,
            Some(1),
            Some(2)
        )?,
        Some(DataType::Double)
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![Call::Signed(
            DataType::HugeInt,
            DataType::HugeInt,
            Some(1),
            Some(2)
        )]
    );
    for unsigned in [0, 1, i128::MAX as u128, (i128::MAX as u128) + 1, u128::MAX] {
        calls.lock().unwrap().clear();
        assert_eq!(
            types.try_common_type_with_literals(
                &DataType::HugeInt,
                &DataType::UHugeInt,
                Some(IntegerLiteral::Signed(1)),
                Some(IntegerLiteral::Unsigned(unsigned))
            )?,
            Some(DataType::Double)
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                Call::Ordinary(DataType::HugeInt, DataType::UHugeInt),
                Call::Ordinary(DataType::UHugeInt, DataType::HugeInt)
            ]
        );
    }
    calls.lock().unwrap().clear();
    for (ty, hint) in [
        (DataType::HugeInt, IntegerLiteral::Unsigned(1)),
        (DataType::UHugeInt, IntegerLiteral::Signed(1)),
        (DataType::UTinyInt, IntegerLiteral::Unsigned(256)),
        (DataType::TinyInt, IntegerLiteral::Signed(128)),
        (DataType::Varchar, IntegerLiteral::Unsigned(1)),
        (DataType::Null, IntegerLiteral::Signed(0)),
    ] {
        assert!(matches!(
            types.try_common_type_with_literals(&ty, &DataType::HugeInt, Some(hint), None),
            Err(Error::Bind(_))
        ));
    }
    assert!(calls.lock().unwrap().is_empty());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_case_and_collection_binding_pass_full_hints_to_selected_families() -> Result<()> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut types = TypeRegistry::builtins();
    for family in ["builtin.integer", "builtin.uhugeint"] {
        types.replace(
            family,
            Arc::new(Full(
                Legacy {
                    calls: calls.clone(),
                    target: DataType::Double,
                },
                false,
            )),
        )?;
    }
    let mut c = DatabaseBuilder::new()
        .types(Arc::new(types))
        .build()?
        .connect();
    let a = Some(IntegerLiteral::Signed(1));
    let b = Some(IntegerLiteral::Unsigned(u128::MAX));
    for expression in [
        "CASE WHEN false THEN 340282366920938463463374607431768211455 ELSE 1 END",
        "[1,340282366920938463463374607431768211455]",
    ] {
        calls.lock().unwrap().clear();
        let target = if expression.starts_with('[') {
            "DOUBLE[]"
        } else {
            "DOUBLE"
        };
        assert_eq!(
            c.query(&format!("SELECT typeof({expression})"))?.rows,
            vec![vec![Value::Varchar(target.into())]]
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                Call::Full(DataType::Integer, DataType::UHugeInt, a, b),
                Call::Full(DataType::UHugeInt, DataType::Integer, b, a)
            ]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn values_binding_retains_selected_full_hints_but_not_parameter_or_assignment_privilege()
-> Result<()> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut types = TypeRegistry::builtins();
    for family in ["builtin.integer", "builtin.uhugeint"] {
        types.replace(
            family,
            Arc::new(Full(
                Legacy {
                    calls: calls.clone(),
                    target: DataType::Double,
                },
                false,
            )),
        )?;
    }
    let mut c = DatabaseBuilder::new()
        .types(Arc::new(types))
        .build()?
        .connect();
    assert_eq!(c.query("SELECT typeof(a) FROM (VALUES(1::INTEGER),(340282366920938463463374607431768211455))t(a)")?.rows,vec![vec![Value::Varchar("DOUBLE".into())];2]);
    let wide = Some(IntegerLiteral::Unsigned(u128::MAX));
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            Call::Full(DataType::Integer, DataType::UHugeInt, None, wide),
            Call::Full(DataType::UHugeInt, DataType::Integer, wide, None)
        ]
    );
    let p = c.prepare("SELECT typeof(a) FROM (VALUES(1::INTEGER),($1))t(a)")?;
    calls.lock().unwrap().clear();
    assert_eq!(
        c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?.rows,
        vec![vec![Value::Varchar("DOUBLE".into())]; 2]
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            Call::Ordinary(DataType::Integer, DataType::UHugeInt),
            Call::Ordinary(DataType::UHugeInt, DataType::Integer)
        ]
    );
    c.execute("CREATE TABLE assigned(u UHUGEINT)")?;
    calls.lock().unwrap().clear();
    c.execute("INSERT INTO assigned VALUES(1),(340282366920938463463374607431768211455)")?;
    assert!(calls.lock().unwrap().is_empty());
    Ok(())
}

#[derive(Debug)]
struct RejectedCombination(u8);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RejectedCombination {
    fn error(&self) -> Error {
        match self.0 {
            0 => Error::Unsupported("selected missing combination capability".into()),
            1 => Error::Internal("selected combination invariant".into()),
            2 => Error::Resource("selected combination budget".into()),
            3 => Error::Interrupted,
            _ => Error::Bind("selected combination rejection".into()),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for RejectedCombination {
    fn name(&self) -> &'static str {
        "selected-rejected-combination"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        TypeRegistry::builtins().bind(ty).map(|_| ())
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        TypeRegistry::builtins().bind(ty)?.validate(value, query)
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Err(self.error())
    }
    fn compare(&self, _: &DataType, _: &Value, _: &Value, _: &QueryContext) -> Result<Ordering> {
        unreachable!("binding rejects before execution")
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        _: &mut KeyWriter<'_>,
        _: &QueryContext,
    ) -> Result<()> {
        unreachable!("binding rejects before execution")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn values_no_common_type_category_does_not_replace_selected_adapter_failures() -> Result<()> {
    for code in 0..5 {
        let mut types = TypeRegistry::builtins();
        types.replace("builtin.integer", Arc::new(RejectedCombination(code)))?;
        let mut c = DatabaseBuilder::new()
            .types(Arc::new(types))
            .build()?
            .connect();
        let error = c
            .query("SELECT a FROM(VALUES(1::INTEGER),(DATE '2024-01-01'))t(a)")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            RejectedCombination(code).error().to_string()
        );
        assert!(!matches!(error, Error::NotImplemented(_)));
    }
    Ok(())
}
