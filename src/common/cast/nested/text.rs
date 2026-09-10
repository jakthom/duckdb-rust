//! Container text retains selected child VARCHAR casts, independently of
//! diagnostic Display. Layout/escaping follows pinned vector_cast_helpers and
//! the LIST/ARRAY/STRUCT/TUPLE/MAP/UNION string-cast adapters.
use super::*;

#[derive(Debug)]
pub(super) struct NestedTextCast {
    metadata: Arc<NestedType>,
    children: Vec<BoundCast>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NestedTextCast {
    pub(super) fn bind(
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &super::super::super::type_registry::TypeRegistry,
    ) -> Result<Arc<dyn CastFunction>> {
        let DataType::Nested(metadata) = &spec.source else {
            return Err(Error::Bind("nested text source metadata".into()));
        };
        let children = metadata
            .children()
            .into_iter()
            .map(|source| casts.bind(source, &DataType::Varchar, spec.mode, types))
            .collect::<Result<_>>()?;
        Ok(Arc::new(Self {
            metadata: metadata.clone(),
            children,
        }))
    }
    fn child(
        &self,
        index: usize,
        value: &Value,
        quote: bool,
        output: &mut String,
        behavior: CastBehavior,
        query: &QueryContext,
    ) -> CastResult<()> {
        let cast = self
            .children
            .get(index)
            .ok_or_else(|| Error::Internal("nested text child binding".into()))?;
        match cast.attempt(value, behavior, query)? {
            Value::Null => append(output, "NULL", query)?,
            Value::Varchar(text) if quote => append_quoted(output, &text, false, query)?,
            Value::Varchar(text) => append(output, &text, query)?,
            _ => {
                return Err(Error::Internal("nested text cast returned non-VARCHAR".into()).into());
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for NestedTextCast {
    fn name(&self) -> &'static str {
        "selected-nested-text-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Nested(self.metadata.clone())
            && spec.target == DataType::Varchar
            && spec.mode != CastMode::Implicit
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        self.cast_attempt(value, spec, CastBehavior::Strict, query)
            .map_err(CastFailure::into_error)
    }
    fn cast_attempt(
        &self,
        value: &Value,
        _: &CastSpec,
        behavior: CastBehavior,
        query: &QueryContext,
    ) -> CastResult<Value> {
        crate::common::temporal::check_cast_text_renderable(value, &mut || query.check())?;
        let Value::Nested(nested) = value else {
            return Err(Error::Internal("nested text input payload".into()).into());
        };
        let quote = |ty: &DataType| !matches!(ty, DataType::Nested(_));
        let mut output = String::new();
        match (&*self.metadata, &nested.payload) {
            (
                NestedType::List(child) | NestedType::Array { element: child, .. },
                NestedPayload::Sequence(values),
            ) => {
                append(&mut output, "[", query)?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        append(&mut output, ", ", query)?;
                    }
                    self.child(0, value, quote(child), &mut output, behavior, query)?;
                }
                append(&mut output, "]", query)?;
            }
            (NestedType::Struct(fields), NestedPayload::Struct(values)) => {
                append(&mut output, "{", query)?;
                for (index, ((name, ty), value)) in fields.iter().zip(values).enumerate() {
                    if index > 0 {
                        append(&mut output, ", ", query)?;
                    }
                    append_quoted(&mut output, name, true, query)?;
                    append(&mut output, ": ", query)?;
                    self.child(index, value, quote(ty), &mut output, behavior, query)?;
                }
                append(&mut output, "}", query)?;
            }
            (NestedType::Tuple(fields), NestedPayload::Struct(values)) => {
                append(&mut output, "(", query)?;
                for (index, (ty, value)) in fields.iter().zip(values).enumerate() {
                    if index > 0 {
                        append(&mut output, ", ", query)?;
                    }
                    self.child(index, value, quote(ty), &mut output, behavior, query)?;
                }
                if fields.len() == 1 {
                    append(&mut output, ",", query)?;
                }
                append(&mut output, ")", query)?;
            }
            (NestedType::Map { key, value }, NestedPayload::Map(entries)) => {
                append(&mut output, "{", query)?;
                for (index, (k, v)) in entries.iter().enumerate() {
                    if index > 0 {
                        append(&mut output, ", ", query)?;
                    }
                    self.child(0, k, quote(key), &mut output, behavior, query)?;
                    append(&mut output, "=", query)?;
                    self.child(1, v, quote(value), &mut output, behavior, query)?;
                }
                append(&mut output, "}", query)?;
            }
            (NestedType::Union(_), NestedPayload::Union { tag, value }) => {
                self.child(*tag, value, false, &mut output, behavior, query)?;
            }
            _ => {
                return Err(Error::Internal("nested text metadata/payload mismatch".into()).into());
            }
        }
        Ok(Value::Varchar(output))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append(output: &mut String, text: &str, query: &QueryContext) -> Result<()> {
    query.check()?;
    output
        .try_reserve(text.len())
        .map_err(|_| Error::Resource("nested text allocation failed".into()))?;
    output.push_str(text);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_quoted(output: &mut String, text: &str, key: bool, query: &QueryContext) -> Result<()> {
    let mut quote = key
        || text.is_empty()
        || text.eq_ignore_ascii_case("null")
        || text.as_bytes().first().is_some_and(u8::is_ascii_whitespace)
        || text.as_bytes().last().is_some_and(u8::is_ascii_whitespace);
    for (index, byte) in text.bytes().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        quote |= matches!(
            byte,
            b'"' | b'\'' | b'(' | b')' | b',' | b':' | b'=' | b'[' | b']' | b'{' | b'}'
        );
    }
    if !quote {
        return append(output, text, query);
    }
    append(output, "'", query)?;
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if matches!(byte, b'\'' | b'\\') {
            append(output, &text[start..index], query)?;
            append(output, "\\", query)?;
            start = index;
        }
    }
    append(output, &text[start..], query)?;
    append(output, "'", query)
}
