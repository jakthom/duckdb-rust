//! Container text retains selected child VARCHAR casts, independently of
//! diagnostic Display. Layout/escaping follows pinned vector_cast_helpers and
//! the LIST/ARRAY/STRUCT/TUPLE/MAP/UNION string-cast adapters.
use super::*;

#[derive(Debug)]
pub(super) struct NestedTextCast {
    metadata: Arc<NestedType>,
    children: Vec<BoundCast>,
    plain_children: Vec<bool>,
    field_keys: Vec<String>,
    needs_renderability_check: bool,
}

struct RenderContext<'a> {
    behavior: CastBehavior,
    source_validated: bool,
    query: &'a QueryContext,
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
            .collect::<Result<Vec<_>>>()?;
        let plain_children = children
            .iter()
            .map(BoundCast::can_preserve_plain_varchar_storage)
            .collect();
        let field_keys = match metadata.as_ref() {
            NestedType::Struct(fields) | NestedType::Object(fields)
                if !metadata.is_positional_struct() =>
            {
                fields
                    .iter()
                    .map(|(name, _)| encode_field_key(name))
                    .collect::<Result<_>>()?
            }
            _ => Vec::new(),
        };
        Ok(Arc::new(Self {
            metadata: metadata.clone(),
            children,
            plain_children,
            field_keys,
            needs_renderability_check: metadata.children().into_iter().any(type_may_be_temporal),
        }))
    }
    fn child(
        &self,
        index: usize,
        value: &Value,
        quote: bool,
        output: &mut String,
        render: &RenderContext<'_>,
    ) -> CastResult<()> {
        let cast = self
            .children
            .get(index)
            .ok_or_else(|| Error::Internal("nested text child binding".into()))?;
        if self.plain_children.get(index).copied() == Some(true) {
            match value {
                Value::Null => append(output, "NULL", render.query)?,
                Value::Varchar(text) if quote => append_quoted(output, text, false, render.query)?,
                Value::Varchar(text) => append(output, text, render.query)?,
                _ => {
                    return Err(Error::Internal(
                        "plain VARCHAR cast received another value".into(),
                    )
                    .into());
                }
            }
            return Ok(());
        }
        let value = if render.source_validated && cast.source_has_recursive_builtin_validation() {
            cast.attempt_validated(value, render.behavior, render.query)?
        } else {
            cast.attempt(value, render.behavior, render.query)?
        };
        match value {
            Value::Null => append(output, "NULL", render.query)?,
            Value::Varchar(text) if quote => append_quoted(output, &text, false, render.query)?,
            Value::Varchar(text) => append(output, &text, render.query)?,
            _ => {
                return Err(Error::Internal("nested text cast returned non-VARCHAR".into()).into());
            }
        }
        Ok(())
    }

    fn child_vector(
        &self,
        child_index: usize,
        column: &crate::common::vector::Vector,
        row: usize,
        quote: bool,
        output: &mut String,
        render: &RenderContext<'_>,
    ) -> CastResult<()> {
        let cast = self
            .children
            .get(child_index)
            .ok_or_else(|| Error::Internal("nested text child binding".into()))?;
        if self.plain_children.get(child_index).copied() == Some(true)
            && let Some(value) = column.varchar_at_validated(row)
        {
            match value {
                None => append(output, "NULL", render.query)?,
                Some(text) if quote => append_quoted(output, text, false, render.query)?,
                Some(text) => append(output, text, render.query)?,
            }
            return Ok(());
        }
        if render.source_validated
            && let Some(result) =
                cast.append_validated_vector_at(column, row, render.behavior, render.query, output)
        {
            result?;
            return Ok(());
        }
        let value = column
            .value(row)
            .ok_or_else(|| Error::Internal("nested text child row".into()))?;
        self.child(child_index, &value, quote, output, render)
    }

    fn render_columnar_into(
        &self,
        row: crate::common::vector::NestedRowRef<'_>,
        behavior: CastBehavior,
        source_validated: bool,
        query: &QueryContext,
        output: &mut String,
    ) -> CastResult<()> {
        query.check()?;
        let quote = |ty: &DataType| !matches!(ty, DataType::Nested(_));
        let render = RenderContext {
            behavior,
            source_validated,
            query,
        };
        match (&*self.metadata, row) {
            (
                NestedType::List(child),
                crate::common::vector::NestedRowRef::List {
                    child: values,
                    range,
                },
            ) => {
                append(output, "[", query)?;
                for (position, row) in range.enumerate() {
                    if position % 1024 == 0 {
                        query.check()?;
                    }
                    if position > 0 {
                        append(output, ", ", query)?;
                    }
                    self.child_vector(0, values, row, quote(child), output, &render)?;
                }
                append(output, "]", query)?;
            }
            (
                NestedType::Struct(fields),
                crate::common::vector::NestedRowRef::Struct { children, index },
            ) if self.metadata.is_positional_struct() => {
                append(output, "(", query)?;
                for (field, ((_, ty), child)) in fields.iter().zip(children).enumerate() {
                    if field % 1024 == 0 {
                        query.check()?;
                    }
                    if field > 0 {
                        append(output, ", ", query)?;
                    }
                    self.child_vector(field, child, index, quote(ty), output, &render)?;
                }
                if fields.len() == 1 {
                    append(output, ",", query)?;
                }
                append(output, ")", query)?;
            }
            (
                NestedType::Struct(fields),
                crate::common::vector::NestedRowRef::Struct { children, index },
            ) => {
                append(output, "{", query)?;
                for (field, ((_, ty), child)) in fields.iter().zip(children).enumerate() {
                    if field % 1024 == 0 {
                        query.check()?;
                    }
                    if field > 0 {
                        append(output, ", ", query)?;
                    }
                    let key = self
                        .field_keys
                        .get(field)
                        .ok_or_else(|| Error::Internal("nested text field key".into()))?;
                    append_cached(output, key, query)?;
                    append(output, ": ", query)?;
                    self.child_vector(field, child, index, quote(ty), output, &render)?;
                }
                append(output, "}", query)?;
            }
            _ => return Err(Error::Internal("columnar nested text row mismatch".into()).into()),
        }
        Ok(())
    }

    fn render(
        &self,
        value: &Value,
        behavior: CastBehavior,
        source_validated: bool,
        query: &QueryContext,
    ) -> CastResult<Value> {
        let mut output = String::new();
        output
            .try_reserve(16)
            .map_err(|_| Error::Resource("nested text allocation failed".into()))?;
        self.render_into(value, behavior, source_validated, query, &mut output)?;
        Ok(Value::Varchar(output))
    }

    fn render_into(
        &self,
        value: &Value,
        behavior: CastBehavior,
        source_validated: bool,
        query: &QueryContext,
        output: &mut String,
    ) -> CastResult<()> {
        query.check()?;
        if self.needs_renderability_check {
            crate::common::temporal::check_cast_text_renderable(value, &mut || query.check())?;
        }
        let Value::Nested(nested) = value else {
            return Err(Error::Internal("nested text input payload".into()).into());
        };
        let quote = |ty: &DataType| !matches!(ty, DataType::Nested(_));
        let render = RenderContext {
            behavior,
            source_validated,
            query,
        };
        match (&*self.metadata, &nested.payload) {
            (
                NestedType::List(child) | NestedType::Array { element: child, .. },
                NestedPayload::Sequence(values),
            ) => {
                append(output, "[", query)?;
                for (index, value) in values.iter().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    if index > 0 {
                        append(output, ", ", query)?;
                    }
                    self.child(0, value, quote(child), output, &render)?;
                }
                append(output, "]", query)?;
            }
            (NestedType::Struct(fields), NestedPayload::Struct(values))
                if self.metadata.is_positional_struct() =>
            {
                append(output, "(", query)?;
                for (index, (ty, value)) in fields.iter().map(|(_, ty)| ty).zip(values).enumerate()
                {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    if index > 0 {
                        append(output, ", ", query)?;
                    }
                    self.child(index, value, quote(ty), output, &render)?;
                }
                if fields.len() == 1 {
                    append(output, ",", query)?;
                }
                append(output, ")", query)?;
            }
            (
                NestedType::Struct(fields) | NestedType::Object(fields),
                NestedPayload::Struct(values),
            ) => {
                append(output, "{", query)?;
                for (index, ((_, ty), value)) in fields.iter().zip(values).enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    if index > 0 {
                        append(output, ", ", query)?;
                    }
                    let key = self
                        .field_keys
                        .get(index)
                        .ok_or_else(|| Error::Internal("nested text field key".into()))?;
                    append_cached(output, key, query)?;
                    append(output, ": ", query)?;
                    self.child(index, value, quote(ty), output, &render)?;
                }
                append(output, "}", query)?;
            }
            (NestedType::Tuple(fields), NestedPayload::Struct(values)) => {
                append(output, "(", query)?;
                for (index, (ty, value)) in fields.iter().zip(values).enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    if index > 0 {
                        append(output, ", ", query)?;
                    }
                    self.child(index, value, quote(ty), output, &render)?;
                }
                if fields.len() == 1 {
                    append(output, ",", query)?;
                }
                append(output, ")", query)?;
            }
            (NestedType::Map { key, value }, NestedPayload::Map(entries)) => {
                append(output, "{", query)?;
                for (index, (k, v)) in entries.iter().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    if index > 0 {
                        append(output, ", ", query)?;
                    }
                    self.child(0, k, quote(key), output, &render)?;
                    append(output, "=", query)?;
                    self.child(1, v, quote(value), output, &render)?;
                }
                append(output, "}", query)?;
            }
            (NestedType::Union(_), NestedPayload::Union { tag, value }) => {
                self.child(*tag, value, false, output, &render)?;
            }
            _ => {
                return Err(Error::Internal("nested text metadata/payload mismatch".into()).into());
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
        self.render(value, behavior, false, query)
    }
    fn cast_validated_batch(
        &self,
        _: CastAdapterAccess,
        proof: CastValidationProof,
        input: &crate::common::vector::Vector,
        spec: &CastSpec,
        query: &QueryContext,
    ) -> Option<Result<crate::common::vector::Vector>> {
        Some(self.cast_validated_batch_inner(input, spec, proof.source_recursive_builtin, query))
    }
    #[allow(clippy::too_many_arguments)]
    fn cast_validated_vector_at_into(
        &self,
        _: CastAdapterAccess,
        source_has_recursive_builtin_validation: bool,
        input: &crate::common::vector::Vector,
        index: usize,
        spec: &CastSpec,
        behavior: CastBehavior,
        query: &QueryContext,
        output: &mut String,
    ) -> Option<CastResult<()>> {
        if !source_has_recursive_builtin_validation
            || self.needs_renderability_check
            || spec.target != DataType::Varchar
        {
            return None;
        }
        let row = input.nested_row_at(index)?;
        Some(match row {
            crate::common::vector::NestedRowRef::Null => {
                append(output, "NULL", query).map_err(Into::into)
            }
            row @ (crate::common::vector::NestedRowRef::Struct { .. }
            | crate::common::vector::NestedRowRef::List { .. }) => {
                self.render_columnar_into(row, behavior, true, query, output)
            }
            crate::common::vector::NestedRowRef::Scalar(_) => return None,
        })
    }
}

impl NestedTextCast {
    fn cast_validated_batch_inner(
        &self,
        input: &crate::common::vector::Vector,
        spec: &CastSpec,
        source_has_recursive_builtin_validation: bool,
        query: &QueryContext,
    ) -> Result<crate::common::vector::Vector> {
        if spec.target != DataType::Varchar {
            return Err(Error::Internal(
                "nested text batch target is not VARCHAR".into(),
            ));
        }
        let mut arena = String::new();
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(input.len())
            .map_err(|_| Error::Resource("nested text result allocation failed".into()))?;
        for index in 0..input.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let row = input.nested_row_at(index);
            ranges.push(match row {
                Some(crate::common::vector::NestedRowRef::Null) => None,
                Some(
                    row @ (crate::common::vector::NestedRowRef::Struct { .. }
                    | crate::common::vector::NestedRowRef::List { .. }),
                ) if source_has_recursive_builtin_validation && !self.needs_renderability_check => {
                    let start = arena.len();
                    self.render_columnar_into(row, CastBehavior::Strict, true, query, &mut arena)
                        .map_err(CastFailure::into_error)?;
                    Some(start..arena.len())
                }
                _ => {
                    let value = input
                        .value(index)
                        .ok_or_else(|| Error::Internal("nested text input row".into()))?;
                    if value.is_null() {
                        None
                    } else {
                        let start = arena.len();
                        self.render_into(
                            &value,
                            CastBehavior::Strict,
                            source_has_recursive_builtin_validation,
                            query,
                            &mut arena,
                        )
                        .map_err(CastFailure::into_error)?;
                        Some(start..arena.len())
                    }
                }
            });
        }
        query.check()?;
        crate::common::vector::Vector::packed_utf8(Arc::new(arena), ranges)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append(output: &mut String, text: &str, _: &QueryContext) -> Result<()> {
    let required = output
        .len()
        .checked_add(text.len())
        .ok_or_else(|| Error::Resource("nested text allocation failed".into()))?;
    if required > output.capacity() {
        output
            .try_reserve(text.len())
            .map_err(|_| Error::Resource("nested text allocation failed".into()))?;
    }
    output.push_str(text);
    Ok(())
}

fn append_cached(output: &mut String, text: &str, query: &QueryContext) -> Result<()> {
    // Fixed metadata can be pre-encoded, but long names retain the same
    // periodic cancellation boundary as the original quoting scan.
    for _ in (0..text.len()).step_by(1024) {
        query.check()?;
    }
    append(output, text, query)
}

fn encode_field_key(text: &str) -> Result<String> {
    let mut output = String::new();
    append_quoted(&mut output, text, true, &QueryContext::background())?;
    Ok(output)
}

fn type_may_be_temporal(data_type: &DataType) -> bool {
    if data_type.is_temporal() || *data_type == DataType::Date {
        return true;
    }
    match data_type {
        DataType::Nested(metadata) => {
            matches!(metadata.as_ref(), NestedType::Variant)
                || metadata.children().into_iter().any(type_may_be_temporal)
        }
        _ => false,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_quoted(output: &mut String, text: &str, key: bool, query: &QueryContext) -> Result<()> {
    let mut quote = key
        || text.is_empty()
        || text.eq_ignore_ascii_case("null")
        || text.as_bytes().first().is_some_and(u8::is_ascii_whitespace)
        || text.as_bytes().last().is_some_and(u8::is_ascii_whitespace);
    let mut escapes = 0usize;
    for (index, byte) in text.bytes().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        quote |= matches!(
            byte,
            b'"' | b'\'' | b'(' | b')' | b',' | b':' | b'=' | b'[' | b']' | b'{' | b'}'
        );
        if matches!(byte, b'\'' | b'\\') {
            escapes = escapes
                .checked_add(1)
                .ok_or_else(|| Error::Resource("nested text allocation failed".into()))?;
        }
    }
    if !quote {
        return append(output, text, query);
    }
    let additional = text
        .len()
        .checked_add(escapes)
        .and_then(|bytes| bytes.checked_add(2))
        .ok_or_else(|| Error::Resource("nested text allocation failed".into()))?;
    let required = output
        .len()
        .checked_add(additional)
        .ok_or_else(|| Error::Resource("nested text allocation failed".into()))?;
    if required > output.capacity() {
        output
            .try_reserve(additional)
            .map_err(|_| Error::Resource("nested text allocation failed".into()))?;
    }
    output.push('\'');
    if escapes == 0 {
        output.push_str(text);
        output.push('\'');
        return Ok(());
    }
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if matches!(byte, b'\'' | b'\\') {
            output.push_str(&text[start..index]);
            output.push('\\');
            start = index;
        }
    }
    output.push_str(&text[start..]);
    output.push('\'');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_append_preserves_triggers_escapes_unicode_nul_and_cancellation() -> Result<()> {
        let query = QueryContext::background();
        for (text, key, expected) in [
            ("", false, "''"),
            ("null", false, "'null'"),
            (" alpha", false, "' alpha'"),
            ("alpha", false, "alpha"),
            ("a,b", false, "'a,b'"),
            ("a'b", false, "'a\\'b'"),
            ("a\\b", false, "a\\b"),
            ("a,\\b", false, "'a,\\\\b'"),
            ("é🦆", false, "é🦆"),
            ("a\0b", false, "a\0b"),
            ("a'b", true, "'a\\'b'"),
        ] {
            let mut output = "prefix:".to_string();
            append_quoted(&mut output, text, key, &query)?;
            assert_eq!(output, format!("prefix:{expected}"), "input {text:?}");
        }

        let long = format!("{},tail", "é".repeat(1025));
        let mut output = String::new();
        append_quoted(&mut output, &long, false, &query)?;
        assert_eq!(output, format!("'{long}'"));

        let interrupt = crate::parallel::InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 64, usize::MAX)?;
        interrupt.interrupt();
        assert!(matches!(
            append_quoted(&mut String::new(), "a,b", false, &cancelled),
            Err(Error::Interrupted)
        ));
        Ok(())
    }

    #[test]
    fn cached_field_keys_preserve_quoting_unicode_nul_and_cancellation() -> Result<()> {
        let query = QueryContext::background();
        for (text, expected) in [
            ("", "''"),
            ("word", "'word'"),
            ("a'b", "'a\\'b'"),
            ("a\\b", "'a\\\\b'"),
            ("é🦆", "'é🦆'"),
            ("a\0b", "'a\0b'"),
        ] {
            let encoded = encode_field_key(text)?;
            assert_eq!(encoded, expected, "input {text:?}");
            let mut output = "prefix:".to_string();
            append_cached(&mut output, &encoded, &query)?;
            assert_eq!(output, format!("prefix:{expected}"));
        }

        let long = format!("{}'tail", "é".repeat(1025));
        let encoded = encode_field_key(&long)?;
        assert_eq!(encoded, format!("'{}\\'tail'", "é".repeat(1025)));
        let mut output = String::new();
        append_cached(&mut output, &encoded, &query)?;
        assert_eq!(output, encoded);

        let interrupt = crate::parallel::InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 64, usize::MAX)?;
        interrupt.interrupt();
        assert!(matches!(
            append_cached(&mut String::new(), &encoded, &cancelled),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
}
