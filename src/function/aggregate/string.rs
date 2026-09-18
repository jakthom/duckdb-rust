use crate::{
    common::{DataType, Error, Result, Value},
    function::{AggregateBinding, AggregateFunction, AggregateState},
    parallel::QueryContext,
};

/// STRING_AGG captures its constant separator in a bound adapter and retains
/// only the input row. This preserves generic ORDER BY/DISTINCT/FILTER and
/// window paths without a function-specific executor wrapper.
#[derive(Debug)]
pub(super) struct StringAgg(pub(super) &'static str, pub(super) Option<Option<String>>);

impl AggregateFunction for StringAgg {
    fn name(&self) -> &str {
        self.0
    }

    fn argument_types(&self, arguments: &[DataType]) -> Result<Vec<DataType>> {
        match arguments {
            [DataType::Varchar] | [DataType::Null] => Ok(vec![DataType::Varchar]),
            [DataType::Varchar, DataType::Varchar]
            | [DataType::Varchar, DataType::Null]
            | [DataType::Null, DataType::Varchar]
            | [DataType::Null, DataType::Null] => Ok(vec![DataType::Varchar; 2]),
            _ => Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.0
            ))),
        }
    }

    fn constant_arguments(&self, arity: usize) -> &[usize] {
        if arity == 2 { &[1] } else { &[] }
    }

    fn bind(&self, constants: &[Option<Value>]) -> Result<Option<AggregateBinding>> {
        let separator = match constants.len() {
            1 => Some(",".to_owned()),
            2 => match constants.get(1).and_then(Option::as_ref) {
                Some(Value::Varchar(separator)) => Some(separator.clone()),
                Some(Value::Null) => None,
                _ => return Err(Error::Internal("string_agg separator binding".into())),
            },
            _ => return Err(Error::Internal("string_agg argument binding".into())),
        };
        // Development retains the separator expression in its bound tree but
        // rewrites a NULL separator's leading input to typed NULL. The Rust
        // aggregate executor has no hidden constant-parameter lane, so the
        // adapter captures the constant and removes its inert child while
        // preserving the observable no-evaluation rule for the leading input.
        Ok(Some(AggregateBinding {
            function: std::sync::Arc::new(Self(self.0, Some(separator.clone()))),
            retain_arguments: vec![0],
            replacements: separator
                .is_none()
                .then_some((0, Value::Null))
                .into_iter()
                .collect(),
        }))
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let _ = types;
        self.argument_types(arguments)?;
        Ok(DataType::Varchar)
    }

    fn create_state(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.return_type(arguments, types)?;
        Ok(Box::new(StringAggState {
            separator: self.1.clone().unwrap_or_else(|| Some(",".to_owned())),
            ..Default::default()
        }))
    }
}

#[derive(Default)]
struct StringAggState {
    value: String,
    seen: bool,
    separator: Option<String>,
}

impl AggregateState for StringAggState {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        let [input] = arguments else {
            return Err(Error::Internal(
                "string_agg arguments differ from binding".into(),
            ));
        };
        let Some(separator) = self.separator.as_deref() else {
            return Ok(());
        };
        let Value::Varchar(input) = input else {
            if input.is_null() {
                return Ok(());
            }
            return Err(Error::Internal(
                "string_agg input differs from binding".into(),
            ));
        };
        let additional = separator
            .len()
            .checked_add(input.len())
            .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))?;
        self.value
            .len()
            .checked_add(additional)
            .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))?;
        self.value
            .try_reserve(additional)
            .map_err(|_| Error::Resource("string_agg allocation failed".into()))?;
        if self.seen {
            self.value.push_str(separator);
        }
        self.value.push_str(input);
        self.seen = true;
        query.check()?;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(if self.seen {
            Value::Varchar(self.value)
        } else {
            Value::Null
        })
    }
}
