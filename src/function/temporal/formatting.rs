//! Compiled core temporal formats; explicit UTC, never ambient locale/timezone.
use super::*;
use crate::{
    common::cast::CastMode,
    function::{ArgumentEvaluation, ScalarSignature},
};
pub(super) mod format;

#[derive(Debug)]
struct Strftime {
    signature: Option<ScalarSignature>,
    format: Option<format::Format>,
    reversed: bool,
    known_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(Strftime {
            signature: None,
            format: None,
            reversed: false,
            known_null: false,
        }))
        .expect("unique temporal formatting function");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn candidates() -> Vec<ScalarSignature> {
    use DataType::*;
    [Date, Timestamp, TimestampNs, TimestampTz, TimestampTzNs]
        .into_iter()
        .flat_map(|kind| {
            [
                ScalarSignature {
                    arguments: vec![kind.clone(), Varchar],
                    return_type: Varchar,
                    argument_names: Some(vec!["data".into(), "format".into()]),
                },
                ScalarSignature {
                    arguments: vec![Varchar, kind],
                    return_type: Varchar,
                    argument_names: Some(vec!["format".into(), "data".into()]),
                },
            ]
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Strftime {
    fn name(&self) -> &str {
        "strftime"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        let candidates = candidates();
        ScalarSignature::validate_candidates(self.name(), &candidates, query)?;
        let chosen = arguments.select_overload(self.name(), &candidates)?;
        let signature = ScalarSignature::selected(&candidates, chosen)?.clone();
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal("selected strftime argument count".into()));
        }
        let reversed = signature.arguments[0] == DataType::Varchar;
        let known_null = arguments.is_provably_null(0)? || arguments.is_provably_null(1)?;
        let format = if known_null {
            None
        } else {
            let index = usize::from(!reversed);
            if !arguments.is_closed(index)? {
                return Err(Error::Bind(
                    "The \"format\" argument in function \"strftime\" must be a constant expression".into(),
                ));
            }
            match arguments.constant_as(index, &DataType::Varchar, CastMode::Implicit)? {
                Value::Varchar(text) => Some(format::Format::compile(&text, query)?),
                Value::Null => None,
                _ => {
                    return Err(Error::Internal(
                        "selected strftime format must be VARCHAR".into(),
                    ));
                }
            }
        };
        Ok(Some(Arc::new(Self {
            signature: Some(signature),
            format,
            reversed,
            known_null,
        })))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self.known_null {
            ArgumentEvaluation::TypeOnly
        } else {
            ArgumentEvaluation::NullOnConstant
        }
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        let signature = self
            .signature
            .as_ref()
            .ok_or_else(|| Error::Unsupported("strftime requires selected binding".into()))?;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal("bound strftime arity".into()));
        }
        Ok(signature.arguments.clone())
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let signature = self
            .signature
            .as_ref()
            .ok_or_else(|| Error::Unsupported("strftime requires selected binding".into()))?;
        if arguments != signature.arguments {
            return Err(Error::Internal("bound strftime signature changed".into()));
        }
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal("NULL strftime received arguments".into()));
            }
            return Ok(Value::Null);
        }
        if arguments.len() != 2 {
            return Err(Error::Internal("strftime argument count".into()));
        }
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let Some(format) = &self.format else {
            return Ok(Value::Null);
        };
        format
            .render(&arguments[usize::from(self.reversed)], query)
            .map(Value::Varchar)
    }
}
