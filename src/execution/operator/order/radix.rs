use super::*;
use crate::{
    common::{Error, Value, type_registry::OrderingRepresentation},
    execution::subquery::PreparedExpression,
    parallel::QueryContext,
};

/// Stable integer radix sorting; other signatures retain comparison sorting.
/// Integer ordering must be advertised independently of equality capabilities.
#[derive(Debug, Default)]
pub struct RadixSort;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SortAlgorithm for RadixSort {
    fn name(&self) -> &'static str {
        "integer-radix-sort"
    }
    fn sort(
        &self,
        input: &mut dyn BatchStream,
        order: &[OrderExpr],
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        for item in order {
            if !item.expression.is_pure_and_total()
                || context
                    .query
                    .types()
                    .bind(&item.expression.data_type)?
                    .ordering_representation()
                    != OrderingRepresentation::SignedInteger
            {
                return ComparisonSort.sort(input, order, context);
            }
        }
        let expressions = order
            .iter()
            .map(|o| PreparedExpression::new(&o.expression))
            .collect::<Vec<_>>();
        let mut columns: Vec<_> = order.iter().map(|_| IntegerColumn::default()).collect();
        let mut batches = Vec::new();
        let mut addresses = Vec::new();
        while let Some(batch) = input.next(context.query.batch_size())? {
            context
                .query
                .check_rows(addresses.len().saturating_add(batch.len()))?;
            for (column, expression) in columns.iter_mut().zip(&expressions) {
                let values = expression.evaluate_batch(&batch, context)?;
                for (position, value) in values.values().enumerate() {
                    if position % 1024 == 0 {
                        context.query.check()?;
                    }
                    match value {
                        Value::Integer(value) => column.push(Some(*value)),
                        Value::Null => column.push(None),
                        _ => {
                            return Err(Error::Internal(
                                "integer sort expression returned another representation".into(),
                            ));
                        }
                    }
                }
            }
            addresses.extend((0..batch.len()).map(|row| (batches.len(), row)));
            batches.push(batch);
            context.query.check()?;
        }
        let mut permutation: Vec<_> = (0..addresses.len()).collect();
        let mut scratch = vec![0; addresses.len()];
        for (column, order) in columns.iter().zip(order).rev() {
            let range = (column.maximum as u128).wrapping_sub(column.minimum as u128);
            let bytes = (128 - range.leading_zeros()).div_ceil(8);
            let offsets = column
                .values
                .iter()
                .zip(&column.valid)
                .enumerate()
                .map(|(position, (&value, &valid))| {
                    if position % 1024 == 0 {
                        context.query.check()?;
                    }
                    Ok(if !valid {
                        0
                    } else if order.descending {
                        (column.maximum as u128).wrapping_sub(value as u128)
                    } else {
                        (value as u128).wrapping_sub(column.minimum as u128)
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            for byte in 0..bytes {
                pass(&mut permutation, &mut scratch, context.query, |row| {
                    (offsets[row] >> (byte * 8)) as u8
                })?;
            }
            if column.has_null && column.has_value {
                pass(&mut permutation, &mut scratch, context.query, |row| {
                    u8::from(column.valid[row] == order.nulls_first)
                })?;
            }
        }
        let mut rows = Vec::with_capacity(addresses.len());
        for (position, index) in permutation.into_iter().enumerate() {
            if position % 1024 == 0 {
                context.query.check()?;
            }
            let (batch, row) = addresses[index];
            rows.push(
                batches[batch]
                    .columns()
                    .iter()
                    .map(|column| column.get(row).expect("retained sort row").clone())
                    .collect(),
            );
        }
        context.query.check()?;
        Ok(rows)
    }
}

#[derive(Default)]
struct IntegerColumn {
    values: Vec<i128>,
    valid: Vec<bool>,
    minimum: i128,
    maximum: i128,
    has_value: bool,
    has_null: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IntegerColumn {
    fn push(&mut self, value: Option<i128>) {
        if let Some(value) = value {
            if !self.has_value {
                self.minimum = value;
                self.maximum = value;
            } else {
                self.minimum = self.minimum.min(value);
                self.maximum = self.maximum.max(value);
            }
            self.has_value = true;
        } else {
            self.has_null = true;
        }
        self.valid.push(value.is_some());
        self.values.push(value.unwrap_or(0));
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn pass(
    permutation: &mut Vec<usize>,
    scratch: &mut Vec<usize>,
    query: &QueryContext,
    digit: impl Fn(usize) -> u8,
) -> Result<()> {
    query.check()?;
    let mut offsets = [0usize; 256];
    for (position, &row) in permutation.iter().enumerate() {
        if position % 1024 == 0 {
            query.check()?;
        }
        offsets[digit(row) as usize] += 1;
    }
    if offsets.iter().filter(|&&count| count != 0).count() <= 1 {
        return Ok(());
    }
    let mut total = 0;
    for count in &mut offsets {
        let next = total + *count;
        *count = total;
        total = next;
    }
    for (position, &row) in permutation.iter().enumerate() {
        if position % 1024 == 0 {
            query.check()?;
        }
        let offset = &mut offsets[digit(row) as usize];
        scratch[*offset] = row;
        *offset += 1;
    }
    std::mem::swap(permutation, scratch);
    query.check()
}
