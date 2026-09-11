//! Composite adapters retain the selected child semantics at binding, even
//! when execution carries a different registry. No built-in fallback is used.
use super::*;
use duckdb_rust::common::{
    cast::{BoundCast, CastFunction},
    type_registry::{BoundType, KeyRepresentation, KeyWriter},
    vector::Vector,
};

const FAMILY: &str = "test.wrapper";

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn wrapped(child: DataType) -> DataType {
    DataType::extension(FAMILY, vec![TypeParameter::Type(Box::new(child))])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn child(data_type: &DataType) -> Result<&DataType> {
    if let DataType::Extension(identity) = data_type
        && identity.name == FAMILY
        && let [TypeParameter::Type(child)] = identity.parameters.as_slice()
    {
        return Ok(child);
    }
    Err(Error::Bind("wrapper requires one child type".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn child_value(data_type: &DataType, value: &Value) -> Result<Value> {
    let Value::Extension(value) = value else {
        return Err(Error::Conversion("expected wrapped bytes".into()));
    };
    Ok(Value::extension(
        child(data_type)?.clone(),
        value.bytes.clone(),
    ))
}

#[derive(Debug, Default)]
struct Wrapper {
    child: Option<BoundType>,
    invalid_key: bool,
    fail_binding: bool,
    directional: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for Wrapper {
    fn name(&self) -> &'static str {
        self.child
            .as_ref()
            .map_or("wrapper-factory", BoundType::adapter)
    }
    fn bind_type(
        &self,
        data_type: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<Arc<dyn TypeAdapter>>> {
        if self.fail_binding {
            return Err(Error::Bind("child specialization failed".into()));
        }
        Ok(Some(Arc::new(Self {
            child: Some(types.bind(child(data_type)?)?),
            invalid_key: self.invalid_key,
            fail_binding: false,
            directional: self.directional,
        })))
    }
    fn key_representation(&self, _: &DataType) -> KeyRepresentation {
        if self.child.is_some() && self.invalid_key {
            KeyRepresentation::Integer
        } else {
            KeyRepresentation::CanonicalBytes
        }
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        child(data_type).map(|_| ())
    }
    fn validate_value(&self, data_type: &DataType, value: &Value, q: &QueryContext) -> Result<()> {
        self.child
            .as_ref()
            .expect("bound child")
            .validate(&child_value(data_type, value)?, q)
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Ok(None)
    }
    fn common_type_with_registry(
        &self,
        left: &DataType,
        right: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        if right.family() != FAMILY {
            return Ok(None);
        }
        if self.directional {
            return Ok(Some(left.clone()));
        }
        Ok(Some(wrapped(
            types.common_type(child(left)?, child(right)?)?,
        )))
    }
    fn compare(
        &self,
        data_type: &DataType,
        left: &Value,
        right: &Value,
        q: &QueryContext,
    ) -> Result<Ordering> {
        self.child.as_ref().expect("bound child").compare(
            &child_value(data_type, left)?,
            &child_value(data_type, right)?,
            q,
        )
    }
    fn write_key(
        &self,
        data_type: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        let mut bytes = Vec::new();
        self.child.as_ref().expect("bound child").append_key(
            &child_value(data_type, value)?,
            &mut bytes,
            q,
        )?;
        output.extend_from_slice(&bytes)
    }
}

#[derive(Debug)]
struct WrapperCast {
    child: Option<BoundCast>,
    invalid_bound_signature: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for WrapperCast {
    fn coercion_cost_with_registry(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &TypeRegistry,
    ) -> Result<Option<u32>> {
        casts.coercion_cost_with_types(child(&spec.source)?, &spec.target, spec.mode, types)
    }
    fn name(&self) -> &'static str {
        self.child
            .as_ref()
            .map_or("wrapper-cast-factory", BoundCast::adapter)
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        !(self.child.is_some() && self.invalid_bound_signature)
            && spec.source.family() == FAMILY
            && spec.target == DataType::Varchar
    }
    fn bind_cast(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &TypeRegistry,
    ) -> Result<Option<Arc<dyn CastFunction>>> {
        Ok(Some(Arc::new(Self {
            child: Some(casts.bind(child(&spec.source)?, &spec.target, spec.mode, types)?),
            invalid_bound_signature: self.invalid_bound_signature,
        })))
    }
    fn cast(&self, value: &Value, spec: &CastSpec, q: &QueryContext) -> Result<Value> {
        self.child
            .as_ref()
            .expect("bound child cast")
            .apply(&child_value(&spec.source, value)?, q)
    }
}

#[derive(Debug)]
struct AlternateAsciiCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for AlternateAsciiCast {
    fn name(&self) -> &'static str {
        "alternate-ascii-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        AsciiCast.supports(spec)
    }
    fn cast(&self, value: &Value, spec: &CastSpec, q: &QueryContext) -> Result<Value> {
        AsciiCast.cast(value, spec, q)
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn composite_binding_retains_children_for_validation_comparison_keys_and_casts() -> Result<()> {
    let (types, mut casts) = composition(Arc::new(MaterializedAscii))?;
    let mut types = (*types).clone();
    types.register(FAMILY, Arc::new(Wrapper::default()))?;
    let data_type = wrapped(ascii::data_type(64)?);
    let bound = types.bind(&data_type)?;
    let cast_spec = CastSpec {
        source: data_type.clone(),
        target: DataType::Varchar,
        mode: CastMode::Explicit,
    };
    casts.register(
        cast_spec.clone(),
        Arc::new(WrapperCast {
            child: None,
            invalid_bound_signature: false,
        }),
    )?;
    let cast = casts.bind(&data_type, &DataType::Varchar, CastMode::Explicit, &types)?;
    types.replace(ascii::FAMILY, Arc::new(StreamingAscii))?;
    casts.replace(
        CastSpec {
            source: ascii::data_type(64)?,
            target: DataType::Varchar,
            mode: CastMode::Explicit,
        },
        Arc::new(AlternateAsciiCast),
    )?;
    assert_eq!(bound.adapter(), "materialized-ascii-ci");
    assert_eq!(types.bind(&data_type)?.adapter(), "streaming-ascii-ci");
    assert_eq!(cast.adapter(), "ascii-ci-cast");
    assert_eq!(
        casts
            .bind(&data_type, &DataType::Varchar, CastMode::Explicit, &types)?
            .adapter(),
        "alternate-ascii-cast"
    );
    // Neither execution registry contains the child family or its conversions.
    let q = QueryContext::background();
    let a = Value::extension(data_type.clone(), b"AbC".to_vec());
    let b = Value::extension(data_type.clone(), b"aBc".to_vec());
    assert_eq!(bound.compare(&a, &b, &q)?, Ordering::Equal);
    let (mut a_key, mut b_key) = (Vec::new(), Vec::new());
    bound.append_key(&a, &mut a_key, &q)?;
    bound.append_key(&b, &mut b_key, &q)?;
    assert_eq!(a_key, b_key);
    let invalid = Value::extension(data_type.clone(), "é".as_bytes().to_vec());
    assert!(bound.validate(&invalid, &q).is_err());
    assert!(cast.apply(&invalid, &q).is_err());
    assert_eq!(cast.apply(&a, &q)?, Value::Varchar("AbC".into()));
    assert_eq!(cast.apply(&Value::Null, &q)?, Value::Null);
    let vector = Vector::flat(data_type, vec![a, Value::Null, b])?;
    assert_eq!(
        cast.apply_batch(&vector, &q)?
            .values()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            Value::Varchar("AbC".into()),
            Value::Null,
            Value::Varchar("aBc".into())
        ]
    );
    assert_eq!(
        types.common_type(&wrapped(DataType::Integer), &wrapped(DataType::BigInt))?,
        wrapped(DataType::BigInt)
    );
    types.replace(
        FAMILY,
        Arc::new(Wrapper {
            directional: true,
            ..Wrapper::default()
        }),
    )?;
    assert_eq!(
        types.common_type(&wrapped(DataType::Integer), &wrapped(DataType::BigInt))?,
        wrapped(DataType::Integer)
    );
    assert_eq!(
        types.common_type(&wrapped(DataType::BigInt), &wrapped(DataType::Integer))?,
        wrapped(DataType::BigInt)
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn composite_binding_rejects_missing_children_invalid_capabilities_and_cast_factories() -> Result<()>
{
    let mut types = TypeRegistry::builtins();
    types.register(FAMILY, Arc::new(Wrapper::default()))?;
    let data_type = wrapped(ascii::data_type(64)?);
    assert!(matches!(types.bind(&data_type), Err(Error::Unsupported(_))));
    types.register(ascii::FAMILY, Arc::new(MaterializedAscii))?;
    for factory in [
        Wrapper {
            invalid_key: true,
            ..Wrapper::default()
        },
        Wrapper {
            fail_binding: true,
            ..Wrapper::default()
        },
    ] {
        types.replace(FAMILY, Arc::new(factory))?;
        assert!(matches!(types.bind(&data_type), Err(Error::Bind(_))));
    }
    types.replace(FAMILY, Arc::new(Wrapper::default()))?;
    let spec = CastSpec {
        source: data_type.clone(),
        target: DataType::Varchar,
        mode: CastMode::Explicit,
    };
    let mut casts = CastRegistry::builtins();
    casts.register(
        spec.clone(),
        Arc::new(WrapperCast {
            child: None,
            invalid_bound_signature: false,
        }),
    )?;
    assert_eq!(
        casts.coercion_cost_with_types(
            &data_type,
            &DataType::Varchar,
            CastMode::Explicit,
            &types
        )?,
        None
    );
    assert!(matches!(
        casts.bind(&data_type, &DataType::Varchar, CastMode::Explicit, &types),
        Err(Error::Bind(_))
    ));
    casts.register(
        CastSpec {
            source: ascii::data_type(64)?,
            target: DataType::Varchar,
            mode: CastMode::Explicit,
        },
        Arc::new(AsciiCast),
    )?;
    assert!(
        casts
            .coercion_cost_with_types(&data_type, &DataType::Varchar, CastMode::Explicit, &types)?
            .is_some()
    );
    casts.replace(
        spec,
        Arc::new(WrapperCast {
            child: None,
            invalid_bound_signature: true,
        }),
    )?;
    assert!(matches!(
        casts.bind(&data_type, &DataType::Varchar, CastMode::Explicit, &types),
        Err(Error::Bind(_))
    ));
    Ok(())
}
