use super::*;
use crate::common::NestedType;
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn nested_access(&self, value: BoundExpr, key: BoundExpr) -> Result<BoundExpr> {
        let DataType::Nested(metadata) = &value.data_type else {
            return Err(Error::Bind(
                "nested accessor requires a nested value".into(),
            ));
        };
        let name = match metadata.as_ref() {
            NestedType::Map { .. } => "map_extract_value",
            NestedType::List(_) | NestedType::Array { .. } => "list_extract",
            NestedType::Struct(_) => "struct_extract",
            NestedType::Union(_) => "union_extract",
            _ => return Err(Error::Bind("invalid nested accessor".into())),
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
            let child = arguments.iter().try_fold(DataType::Null, |ty, arg| {
                self.context.query.types().common_type(&ty, &arg.data_type)
            })?;
            let arguments = arguments
                .into_iter()
                .map(|argument| {
                    argument.cast(
                        child.clone(),
                        CastMode::Implicit,
                        self.context.casts,
                        self.context.query.types(),
                    )
                })
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
