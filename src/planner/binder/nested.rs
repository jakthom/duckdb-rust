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
            let child = self.ordered_combination_type(
                &arguments,
                super::coercion::CombinationSequence::Collection,
            )?;
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
