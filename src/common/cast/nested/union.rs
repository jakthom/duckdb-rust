use super::*;

// Target tag, declared source/target child types, and selected coercion cost.
type MemberMapping = (usize, DataType, DataType, u32);

#[derive(Debug, Default)]
pub(super) struct UnionCast {
    children: Vec<BoundCast>,
    tags: Vec<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl UnionCast {
    fn mapping(
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &super::super::super::type_registry::TypeRegistry,
    ) -> Result<Option<Vec<MemberMapping>>> {
        let DataType::Nested(target) = &spec.target else {
            return Ok(None);
        };
        let NestedType::Union(targets) = target.as_ref() else {
            return Ok(None);
        };
        if let DataType::Nested(source) = &spec.source
            && let NestedType::Union(sources) = source.as_ref()
        {
            let mut mapping = Vec::with_capacity(sources.len());
            for (name, source) in sources {
                let Some((tag, (_, target))) = targets
                    .iter()
                    .enumerate()
                    .find(|(_, (target, _))| target.eq_ignore_ascii_case(name))
                else {
                    return Ok(None);
                };
                let Some(cost) =
                    casts.coercion_cost_with_types(source, target, spec.mode, types)?
                else {
                    return Ok(None);
                };
                mapping.push((tag, source.clone(), target.clone(), cost));
            }
            return Ok(Some(mapping));
        }
        let mut best = None;
        let mut ambiguous = false;
        for (tag, (_, target)) in targets.iter().enumerate() {
            let Some(cost) =
                casts.coercion_cost_with_types(&spec.source, target, CastMode::Implicit, types)?
            else {
                continue;
            };
            match &best {
                Some((_, _, _, prior)) if *prior < cost => {}
                Some((_, _, _, prior)) if *prior == cost => ambiguous = true,
                _ => {
                    best = Some((tag, spec.source.clone(), target.clone(), cost));
                    ambiguous = false;
                }
            }
        }
        if ambiguous {
            return Err(Error::Bind("ambiguous UNION member conversion".into()));
        }
        Ok(best.map(|entry| vec![entry]))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for UnionCast {
    fn null_handling(&self, spec: &CastSpec) -> CastNullHandling {
        if matches!(&spec.source,DataType::Nested(metadata) if matches!(metadata.as_ref(),NestedType::Union(_)))
            || spec.source == DataType::Null
        {
            CastNullHandling::Propagate
        } else {
            CastNullHandling::Call
        }
    }
    fn name(&self) -> &'static str {
        "nested-union-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        matches!(&spec.target, DataType::Nested(metadata) if matches!(metadata.as_ref(), NestedType::Union(_)))
    }
    fn coercion_cost_with_registry(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &super::super::super::type_registry::TypeRegistry,
    ) -> Result<Option<u32>> {
        Ok(Self::mapping(spec, casts, types)?.map(|mapping| {
            mapping
                .iter()
                .map(|(_, _, _, cost)| *cost)
                .max()
                .unwrap_or(0)
                .saturating_add(1)
        }))
    }
    fn bind_cast(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &super::super::super::type_registry::TypeRegistry,
    ) -> Result<Option<Arc<dyn CastFunction>>> {
        let mapping = Self::mapping(spec, casts, types)?
            .ok_or_else(|| Error::Bind("no matching UNION member conversion".into()))?;
        let union_source = matches!(&spec.source,DataType::Nested(metadata) if matches!(metadata.as_ref(),NestedType::Union(_)));
        let mode = if union_source {
            spec.mode
        } else {
            CastMode::Implicit
        };
        let children = mapping
            .iter()
            .map(|(_, source, target, _)| casts.bind(source, target, mode, types))
            .collect::<Result<_>>()?;
        Ok(Some(Arc::new(Self {
            children,
            tags: mapping.into_iter().map(|(tag, _, _, _)| tag).collect(),
        })))
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        let (index, value) = if let Value::Nested(nested) = value
            && let NestedPayload::Union { tag, value } = &nested.payload
        {
            (*tag, value)
        } else {
            (0, value)
        };
        let value = self
            .children
            .get(index)
            .ok_or_else(|| Error::Internal("unbound UNION cast child".into()))?
            .apply(value, query)?;
        let tag = *self
            .tags
            .get(index)
            .ok_or_else(|| Error::Internal("unbound UNION cast tag".into()))?;
        NestedValue::value(spec.target.clone(), NestedPayload::Union { tag, value })
    }
}
