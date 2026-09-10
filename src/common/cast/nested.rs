use super::*;
use crate::common::{NestedPayload, NestedType, NestedValue};
mod union;
mod variant;

#[derive(Debug, Default)]
pub struct NestedCast {
    children: Vec<BoundCast>,
    indices: Vec<Option<usize>>,
    target: Option<super::super::type_registry::BoundType>,
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
            (NestedType::Tuple(a), NestedType::Tuple(b)) => a.len() == b.len(),
            (NestedType::Tuple(a), NestedType::Struct(b))
            | (NestedType::Struct(b), NestedType::Tuple(a)) => a.len() == b.len(),
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
        Ok(Some(Arc::new(Self {
            children,
            indices,
            target: Some(types.bind(&spec.target)?),
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
        behavior: CastBehavior,
        query: &QueryContext,
    ) -> CastResult<Value> {
        query.check()?;
        if spec.source == spec.target {
            return Ok(value.clone());
        }
        if spec.target == DataType::Varchar {
            crate::common::temporal::check_cast_text_renderable(value, &mut || query.check())?;
            return Ok(Value::Varchar(value.to_string()));
        }
        let Value::Nested(value) = value else {
            return Err(Error::Conversion("expected nested cast input".into()).into());
        };
        let cast = |index: usize, value: &Value| {
            self.children
                .get(index)
                .ok_or_else(|| Error::Internal("nested cast was not bound".into()))?
                .attempt(value, behavior, query)
        };
        let payload = match &value.payload {
            NestedPayload::Sequence(values) => NestedPayload::Sequence(
                values
                    .iter()
                    .map(|value| cast(0, value))
                    .collect::<CastResult<_>>()?,
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
                    .collect::<CastResult<_>>()?,
            ),
            NestedPayload::Map(entries) => NestedPayload::Map(
                entries
                    .iter()
                    .map(|(key, value)| Ok((cast(0, key)?, cast(1, value)?)))
                    .collect::<CastResult<_>>()?,
            ),
            _ => return Err(Error::Unsupported("nested cast payload".into()).into()),
        };
        let output = NestedValue::value(spec.target.clone(), payload)?;
        // Conversion can create duplicate or NULL MAP keys. These are rejected
        // input conversions, not a malformed adapter result. Infrastructure
        // categories from selected validators remain fatal by default.
        self.target
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound nested cast target".into()))?
            .validate(&output, query)?;
        Ok(output)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    let families = [
        "builtin.list",
        "builtin.array",
        "builtin.struct",
        "builtin.tuple",
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
                .register_family(
                    source,
                    target,
                    if source == "builtin.variant" || target == "builtin.variant" {
                        Arc::new(variant::VariantCast::default())
                    } else if target == "builtin.union" {
                        Arc::new(union::UnionCast::default())
                    } else {
                        Arc::new(NestedCast::default())
                    },
                )
                .expect("unique nested cast family");
        }
    }
    for source in [
        "boolean",
        "tinyint",
        "smallint",
        "integer",
        "bigint",
        "hugeint",
        "utinyint",
        "usmallint",
        "uinteger",
        "ubigint",
        "uhugeint",
        "decimal",
        "float",
        "double",
        "varchar",
        "date",
        "blob",
        "uuid",
        "enum",
        "time",
        "time_ns",
        "time_tz",
        "timestamp",
        "timestamp_s",
        "timestamp_ms",
        "timestamp_ns",
        "timestamp_tz",
        "timestamp_tz_ns",
        "interval",
        "bit",
    ] {
        registry
            .register_family(
                &format!("builtin.{source}"),
                "builtin.union",
                Arc::new(union::UnionCast::default()),
            )
            .expect("unique scalar UNION cast");
        registry
            .register_family(
                &format!("builtin.{source}"),
                "builtin.variant",
                Arc::new(variant::VariantCast::default()),
            )
            .expect("unique scalar VARIANT injection");
        if source != "varchar" {
            registry
                .register_family(
                    "builtin.variant",
                    &format!("builtin.{source}"),
                    Arc::new(variant::VariantCast::default()),
                )
                .expect("unique scalar VARIANT extraction");
        }
    }
}
