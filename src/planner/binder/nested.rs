use super::*;
use crate::common::NestedType;
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn map_constructor(
        &self,
        entries: Vec<(BoundExpr, BoundExpr)>,
    ) -> Result<BoundExpr> {
        let (keys, values) = entries.into_iter().unzip();
        let keys = self.nested_constructor(keys, None)?;
        let values = self.nested_constructor(values, None)?;
        self.scalar_call("map", vec![keys, values])
    }
    fn sequence_type(&self, arguments: &[BoundExpr]) -> Result<DataType> {
        let Some(first) = arguments.first() else {
            return Ok(DataType::Null);
        };
        let types = self.context.query.types();
        let mut child = first.data_type.clone();
        let mut literal = super::coercion::string_literal(first);
        // Template inference is ordered: subsequent untyped NULLs do not
        // change an inferred type, but an initial NULL followed by a string
        // normalizes to concrete VARCHAR and loses string-literal identity.
        for argument in &arguments[1..] {
            if argument.data_type == DataType::Null {
                continue;
            }
            let other_literal = super::coercion::string_literal(argument);
            if literal && other_literal {
                continue;
            }
            let inferred = types.try_common_type(&child, &argument.data_type)?;
            child = if let Some(inferred) = inferred {
                inferred
            } else if literal {
                argument.data_type.clone()
            } else if other_literal && child != DataType::Null {
                child
            } else {
                return Err(Error::Bind(format!(
                    "Cannot combine sequence children of type {child} and {}",
                    argument.data_type
                )));
            };
            literal = false;
        }
        Ok(child)
    }
    pub(super) fn nested_access(&self, value: BoundExpr, key: BoundExpr) -> Result<BoundExpr> {
        let DataType::Nested(metadata) = &value.data_type else {
            return Err(Error::Bind(
                "nested accessor requires a nested value".into(),
            ));
        };
        let name = match metadata.as_ref() {
            NestedType::Map { .. } => "map_extract_value",
            NestedType::List(_) | NestedType::Array { .. } => "list_extract",
            NestedType::Struct(_) | NestedType::Tuple(_) => "struct_extract",
            NestedType::Union(_) => "union_extract",
            NestedType::Variant => "variant_extract",
            NestedType::Object(_) => {
                return Err(Error::Unsupported(
                    "direct access to internal OBJECT metadata".into(),
                ));
            }
        };
        self.scalar_call(name, vec![value, key])
    }
    pub(super) fn nested_constructor(
        &self,
        arguments: Vec<BoundExpr>,
        names: Option<Vec<String>>,
    ) -> Result<BoundExpr> {
        let (data_type, arguments) = if let Some(names) = names {
            (
                NestedType::Struct(
                    names
                        .into_iter()
                        .zip(arguments.iter().map(|arg| arg.data_type.clone()))
                        .collect(),
                )
                .data_type(),
                arguments,
            )
        } else {
            let child = self.sequence_type(&arguments)?;
            let arguments = arguments
                .into_iter()
                .map(|argument| self.combination_cast(argument, &child))
                .collect::<Result<_>>()?;
            (NestedType::List(child).data_type(), arguments)
        };
        self.context.query.types().bind(&data_type)?;
        Ok(BoundExpr {
            data_type: data_type.clone(),
            kind: ExprKind::Scalar(
                Arc::new(crate::function::nested::Constructor(data_type)),
                arguments,
            ),
        })
    }
}
