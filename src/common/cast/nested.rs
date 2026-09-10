use super::*;
use crate::common::{NestedPayload, NestedType, NestedValue};

#[derive(Debug, Default)]
pub struct NestedCast {
    children: Vec<BoundCast>,
    indices: Vec<Option<usize>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for NestedCast {
    fn name(&self) -> &'static str {
        "recursive-nested-cast"
    }
    fn coercion_cost_with_registry(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &super::super::type_registry::TypeRegistry,
    ) -> Result<Option<u32>> {
        if spec.source == spec.target {
            return Ok(Some(0));
        }
        if spec.target == DataType::Varchar {
            return Ok(Some(self.coercion_cost(spec)));
        }
        let (DataType::Nested(a), DataType::Nested(b)) = (&spec.source, &spec.target) else {
            return Ok(None);
        };
        let sources = a.children();
        let mut maximum = 0;
        for (index, target) in b.children().into_iter().enumerate() {
            let source = match (a.as_ref(), b.as_ref()) {
                (NestedType::Struct(a), NestedType::Struct(b)) => a
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(&b[index].0))
                    .map(|(_, ty)| ty)
                    .unwrap_or(&DataType::Null),
                _ => sources[index],
            };
            let Some(cost) = casts.coercion_cost_with_types(source, target, spec.mode, types)?
            else {
                return Ok(None);
            };
            maximum = maximum.max(cost);
        }
        Ok(Some(maximum.saturating_add(1)))
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        if spec.source == spec.target {
            return true;
        }
        if spec.target == DataType::Varchar {
            return spec.mode != CastMode::Implicit;
        }
        let (DataType::Nested(a), DataType::Nested(b)) = (&spec.source, &spec.target) else {
            return false;
        };
        match (a.as_ref(), b.as_ref()) {
            (NestedType::List(_) | NestedType::Array { .. }, NestedType::List(_)) => true,
            (NestedType::List(_) | NestedType::Array { .. }, NestedType::Array { .. }) => {
                spec.mode != CastMode::Implicit
            }
            (NestedType::Struct(a), NestedType::Struct(b)) => a
                .iter()
                .any(|(a, _)| b.iter().any(|(b, _)| a.eq_ignore_ascii_case(b))),
            (NestedType::Map { .. }, NestedType::Map { .. }) => true,
            _ => false,
        }
    }
    fn bind_cast(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &super::super::type_registry::TypeRegistry,
    ) -> Result<Option<Arc<dyn CastFunction>>> {
        if spec.source == spec.target || spec.target == DataType::Varchar {
            return Ok(None);
        }
        let (DataType::Nested(a), DataType::Nested(b)) = (&spec.source, &spec.target) else {
            return Err(Error::Bind("nested cast metadata".into()));
        };
        let indices = match (a.as_ref(), b.as_ref()) {
            (NestedType::Struct(a), NestedType::Struct(b)) => b
                .iter()
                .map(|(name, _)| {
                    a.iter()
                        .position(|(source, _)| source.eq_ignore_ascii_case(name))
                })
                .collect(),
            _ => (0..b.children().len()).map(Some).collect::<Vec<_>>(),
        };
        let sources = a.children();
        let children = indices
            .iter()
            .zip(b.children())
            .map(|(index, target)| {
                casts.bind(
                    index.map(|index| sources[index]).unwrap_or(&DataType::Null),
                    target,
                    spec.mode,
                    types,
                )
            })
            .collect::<Result<_>>()?;
        Ok(Some(Arc::new(Self { children, indices })))
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if spec.source == spec.target {
            return Ok(value.clone());
        }
        if spec.target == DataType::Varchar {
            return Ok(Value::Varchar(value.to_string()));
        }
        let Value::Nested(value) = value else {
            return Err(Error::Conversion("expected nested cast input".into()));
        };
        let cast = |index: usize, value: &Value| {
            self.children
                .get(index)
                .ok_or_else(|| Error::Internal("nested cast was not bound".into()))?
                .apply(value, query)
        };
        let payload = match &value.payload {
            NestedPayload::Sequence(values) => NestedPayload::Sequence(
                values
                    .iter()
                    .map(|value| cast(0, value))
                    .collect::<Result<_>>()?,
            ),
            NestedPayload::Struct(values) => NestedPayload::Struct(
                self.indices
                    .iter()
                    .enumerate()
                    .map(|(index, source)| {
                        cast(
                            index,
                            source.map(|source| &values[source]).unwrap_or(&Value::Null),
                        )
                    })
                    .collect::<Result<_>>()?,
            ),
            NestedPayload::Map(entries) => NestedPayload::Map(
                entries
                    .iter()
                    .map(|(key, value)| Ok((cast(0, key)?, cast(1, value)?)))
                    .collect::<Result<_>>()?,
            ),
            _ => return Err(Error::Unsupported("nested cast payload".into())),
        };
        NestedValue::value(spec.target.clone(), payload)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    let families = [
        "builtin.list",
        "builtin.array",
        "builtin.struct",
        "builtin.map",
        "builtin.union",
        "builtin.variant",
    ];
    for source in families {
        registry
            .register_family("builtin.null", source, Arc::new(StructuralCast))
            .expect("unique nested NULL cast");
        for target in families.into_iter().chain(["builtin.varchar"]) {
            registry
                .register_family(source, target, Arc::new(NestedCast::default()))
                .expect("unique nested cast family");
        }
    }
}
