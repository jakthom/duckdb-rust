use super::*;
use crate::common::vector::{DataChunk, Vector};

/// Scalar fallback shared by built-in and independently registered adapters.
pub fn evaluate_operator_rows<T: OperatorFunction + ?Sized>(
    function: &T,
    signature: &OperatorSignature,
    arguments: &DataChunk,
    query: &QueryContext,
) -> Result<Vector> {
    let mut row = Vec::with_capacity(arguments.columns().len());
    let mut values = Vec::with_capacity(arguments.len());
    for index in 0..arguments.len() {
        query.check()?;
        arguments.read_row(index, &mut row)?;
        values.push(if row.iter().any(Value::is_null) {
            Value::Null
        } else {
            function.evaluate(signature, &row, query)?
        });
    }
    Vector::flat(signature.result.clone(), values)
}

impl BoundOperator {
    /// Validate foreign input and output once at the batch boundary. No
    /// implementation can bypass physical types, logical payload validation,
    /// cardinality or the signature's NULL rules by choosing a batch kernel.
    pub fn apply_batch(&self, arguments: &DataChunk, query: &QueryContext) -> Result<Vector> {
        query.check()?;
        if arguments.columns().len() != self.arguments.len() {
            return Err(Error::Internal(
                "operator batch argument count differs from binding".into(),
            ));
        }
        for (column, data_type) in arguments.columns().iter().zip(&self.arguments) {
            data_type.validate_vector(column, query)?;
        }
        let output = self
            .function
            .evaluate_batch(&self.signature, arguments, query);
        query.check()?;
        let output = output?;
        if output.len() != arguments.len() || output.data_type() != self.result.data_type() {
            return Err(Error::Internal(
                "operator batch result differs from binding".into(),
            ));
        }
        if arguments.columns().iter().all(Vector::all_valid) {
            if !self.signature.nullable
                && !output.all_valid()
                && output.values().any(Value::is_null)
            {
                return Err(Error::Internal(
                    "operator batch violated NULL propagation".into(),
                ));
            }
            self.result
                .validate_vector(&output, query)
                .map_err(|error| match error {
                    Error::Conversion(_) => {
                        Error::Internal("operator returned an invalid logical result".into())
                    }
                    other => other,
                })?;
            return Ok(output);
        }
        for (index, value) in output.values().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let null_input = arguments
                .columns()
                .iter()
                .any(|column| column.get(index).is_some_and(Value::is_null));
            if (null_input && !value.is_null())
                || (!null_input && !self.signature.nullable && value.is_null())
            {
                return Err(Error::Internal(
                    "operator batch violated NULL propagation".into(),
                ));
            }
            if self.result.requires_logical_validation() {
                self.result
                    .validate(value, query)
                    .map_err(|error| match error {
                        Error::Conversion(_) => {
                            Error::Internal("operator returned an invalid logical result".into())
                        }
                        other => other,
                    })?;
            }
        }
        query.check()?;
        Ok(output)
    }
}
