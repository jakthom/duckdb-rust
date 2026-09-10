use super::*;
use crate::common::{
    type_registry::{TypeRegistry, variant::check_categories},
    variant::{Node, invalid},
};

#[derive(Debug, Default)]
pub(super) struct VariantCast {
    registries: Option<(CastRegistry, TypeRegistry)>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl VariantCast {
    fn convert(
        &self,
        node: Node<'_>,
        target: &DataType,
        query: &QueryContext,
        depth: usize,
    ) -> CastResult<Value> {
        query.check()?;
        if depth > 64 {
            return Err(Error::Resource("VARIANT cast depth exceeds 64".into()).into());
        }
        let node = node.resolved()?;
        if node.rank()? == 16 {
            return Ok(Value::Null);
        }
        if *target == NestedType::Variant.data_type() {
            return Ok(node.owned()?);
        }
        if let DataType::Nested(metadata) = target {
            let payload = match metadata.as_ref() {
                NestedType::List(child) | NestedType::Array { element: child, .. } => {
                    if node.rank()? != 14 {
                        return Err(invalid().into());
                    }
                    let values = node.array()?;
                    query.check_rows(values.len())?;
                    if let NestedType::Array { length, .. } = metadata.as_ref()
                        && values.len() != *length
                    {
                        return Err(Error::Conversion(
                            "VARIANT ARRAY cardinality differs from target".into(),
                        )
                        .into());
                    }
                    NestedPayload::Sequence(
                        values
                            .into_iter()
                            .map(|value| self.convert(value, child, query, depth + 1))
                            .collect::<CastResult<_>>()?,
                    )
                }
                NestedType::Struct(fields) => {
                    if node.rank()? != 15 {
                        return Err(invalid().into());
                    }
                    let values = node.object()?;
                    NestedPayload::Struct(
                        fields
                            .iter()
                            .map(|(name, ty)| {
                                let value = values
                                    .iter()
                                    .find(|(key, _)| *key == name)
                                    .ok_or_else(|| {
                                        Error::Conversion(format!(
                                            "VARIANT OBJECT is missing key '{name}'"
                                        ))
                                    })?;
                                self.convert(value.1, ty, query, depth + 1)
                            })
                            .collect::<CastResult<_>>()?,
                    )
                }
                NestedType::Map { key, value } => {
                    if node.rank()? != 14 {
                        return Err(invalid().into());
                    }
                    let entries = node.array()?;
                    query.check_rows(entries.len())?;
                    let mut result = Vec::with_capacity(entries.len());
                    for entry in entries {
                        let fields = entry.object()?;
                        let k = fields
                            .iter()
                            .find(|(name, _)| *name == "key")
                            .ok_or_else(invalid)?;
                        let v = fields
                            .iter()
                            .find(|(name, _)| *name == "value")
                            .ok_or_else(invalid)?;
                        result.push((
                            self.convert(k.1, key, query, depth + 1)?,
                            self.convert(v.1, value, query, depth + 1)?,
                        ));
                    }
                    NestedPayload::Map(result)
                }
                NestedType::Tuple(fields) => {
                    if node.rank()? != 14 {
                        return Err(invalid().into());
                    }
                    let values = node.array()?;
                    if values.len() != fields.len() {
                        return Err(Error::Conversion(
                            "VARIANT ARRAY cardinality differs from TUPLE".into(),
                        ).into());
                    }
                    NestedPayload::Struct(
                        values
                            .into_iter()
                            .zip(fields)
                            .map(|(value, ty)| self.convert(value, ty, query, depth + 1))
                            .collect::<CastResult<_>>()?,
                    )
                }
                NestedType::Union(_) => {
                    return Err(Error::Conversion("Can't convert VARIANT to UNION".into()).into());
                }
                NestedType::Variant => unreachable!("handled VARIANT target"),
            };
            return Ok(NestedValue::value(target.clone(), payload)?);
        }
        if matches!(target, DataType::Enum(_)) {
            return Err(Error::Conversion("Can't convert VARIANT to ENUM".into()).into());
        }
        let (casts, types) = self
            .registries
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound VARIANT cast".into()))?;
        if *target == DataType::Varchar && node.rank()? >= 14 {
            let (source, value) = node.materialized(depth, &|| query.check())?;
            return casts
                .bind(&source, target, CastMode::Explicit, types)?
                .attempt(&value, CastBehavior::Strict, query);
        }
        let Node::Typed(source, value) = node else {
            return Err(
                Error::Conversion(format!("Can't convert VARIANT OBJECT to {target}")).into(),
            );
        };
        if let Value::Enum(value) = value {
            return casts
                .bind(&DataType::Varchar, target, CastMode::Explicit, types)?
                .attempt(
                    &Value::Varchar(value.label()?.to_owned()),
                    CastBehavior::Strict,
                    query,
                );
        }
        casts
            .bind(source, target, CastMode::Explicit, types)
            .map_err(|error| match error {
                Error::Bind(_) => {
                    Error::Conversion(format!("Can't convert VARIANT({source}) to {target}"))
                }
                other => other,
            })?
            .attempt(value, CastBehavior::Strict, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for VariantCast {
    fn name(&self) -> &'static str {
        "dynamic-variant-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source.family() == "builtin.variant" || spec.target.family() == "builtin.variant"
    }
    fn may_return_null(&self, spec: &CastSpec) -> bool {
        spec.source.family() == "builtin.union" && spec.target.family() == "builtin.variant"
    }
    fn bind_cast(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &TypeRegistry,
    ) -> Result<Option<Arc<dyn CastFunction>>> {
        check_categories(&spec.source)?;
        check_categories(&spec.target)?;
        Ok(Some(Arc::new(Self {
            registries: Some((casts.clone(), types.clone())),
        })))
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        self.cast_attempt(value, spec, CastBehavior::Strict, query)
            .map_err(CastFailure::into_error)
    }
    fn cast_attempt(
        &self,
        value: &Value,
        spec: &CastSpec,
        _: CastBehavior,
        query: &QueryContext,
    ) -> CastResult<Value> {
        query.check()?;
        if spec.source == spec.target {
            return Ok(value.clone());
        }
        let output = if spec.target.family() == "builtin.variant" {
            crate::common::variant::inject(&spec.source, value)?
        } else {
            self.convert(Node::Typed(&spec.source, value), &spec.target, query, 0)?
        };
        let (_, types) = self
            .registries
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound VARIANT cast".into()))?;
        // Invalid converted MAP keys or constrained children are ordinary cast
        // failures here, not malformed adapter output at the BoundCast boundary.
        types.bind(&spec.target)?.validate(&output, query)?;
        Ok(output)
    }
}
