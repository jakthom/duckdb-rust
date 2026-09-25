use super::*;

pub(super) struct PreparedWindow {
    pub(super) arguments: RowCollection,
    pub(super) partition: RowCollection,
    pub(super) order: RowCollection,
    pub(super) partition_types: Vec<BoundType>,
    pub(super) order_types: Vec<BoundType>,
    pub(super) filter: Vec<bool>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PreparedWindow {
    pub(super) fn new(
        batches: &[DataChunk],
        window: &WindowExpression,
        context: &ExecutionContext<'_>,
    ) -> Result<Self> {
        let mut result = Self {
            arguments: RowCollection::new(window.arguments.len()),
            partition: RowCollection::new(window.partition.len()),
            order: RowCollection::new(window.order.len()),
            partition_types: window
                .partition
                .iter()
                .map(|e| context.query.types().bind(&e.data_type))
                .collect::<Result<_>>()?,
            order_types: window
                .order
                .iter()
                .map(|e| context.query.types().bind(&e.expression.data_type))
                .collect::<Result<_>>()?,
            filter: Vec::new(),
        };
        let roots = window
            .arguments
            .iter()
            .chain(&window.partition)
            .chain(window.order.iter().map(|key| &key.expression))
            .collect::<Vec<_>>();
        let pure = roots.iter().all(|expr| expr.is_pure_and_total())
            && window
                .filter
                .as_ref()
                .is_none_or(BoundExpr::is_pure_and_total);
        let expressions = roots
            .iter()
            .map(|expr| PreparedExpression::new(expr))
            .collect::<Vec<_>>();
        let root_types = roots
            .iter()
            .map(|expr| context.query.types().bind(&expr.data_type))
            .collect::<Result<Vec<_>>>()?;
        let filter = window.filter.as_ref().map(PreparedExpression::new);
        let filter_type = window
            .filter
            .as_ref()
            .map(|expr| context.query.types().bind(&expr.data_type))
            .transpose()?;
        for batch in batches {
            context.query.check()?;
            if pure {
                let values = expressions
                    .iter()
                    .map(|expr| expr.evaluate_batch(batch, context))
                    .collect::<Result<Vec<_>>>()?;
                let a = window.arguments.len();
                let b = a + window.partition.len();
                result
                    .arguments
                    .append(&DataChunk::new(values[..a].to_vec(), batch.len())?)?;
                result
                    .partition
                    .append(&DataChunk::new(values[a..b].to_vec(), batch.len())?)?;
                result
                    .order
                    .append(&DataChunk::new(values[b..].to_vec(), batch.len())?)?;
                if let Some(filter) = &filter {
                    for value in filter.evaluate_batch(batch, context)?.values() {
                        result.filter.push(value.as_bool()?.unwrap_or(false));
                    }
                } else {
                    result
                        .filter
                        .resize(result.filter.len() + batch.len(), true);
                }
            } else {
                let mut row = Vec::new();
                for index in 0..batch.len() {
                    batch.read_row(index, &mut row)?;
                    let values = expressions
                        .iter()
                        .zip(&root_types)
                        .map(|(expr, data_type)| {
                            let value = expr.evaluate(&row, context)?;
                            data_type.validate(&value, context.query)?;
                            Ok(value)
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let a = window.arguments.len();
                    let b = a + window.partition.len();
                    result.arguments.push(&values[..a])?;
                    result.partition.push(&values[a..b])?;
                    result.order.push(&values[b..])?;
                    result.filter.push(match &filter {
                        Some(filter) => {
                            let value = filter.evaluate(&row, context)?;
                            filter_type
                                .as_ref()
                                .expect("bound filter type")
                                .validate(&value, context.query)?;
                            value.as_bool()?.unwrap_or(false)
                        }
                        None => true,
                    });
                }
            }
        }
        Ok(result)
    }
}
