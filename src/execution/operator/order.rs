use crate::{
    common::{Result, Row},
    execution::ExecutionContext,
    planner::logical::OrderExpr,
};
use std::cmp::Ordering;

pub fn sort(
    rows: Vec<Row>,
    order: &[OrderExpr],
    context: &ExecutionContext<'_>,
) -> Result<Vec<Row>> {
    let types = order
        .iter()
        .map(|o| context.query.types().bind(&o.expression.data_type))
        .collect::<Result<Vec<_>>>()?;
    let expressions = order
        .iter()
        .map(|o| crate::execution::subquery::PreparedExpression::new(&o.expression))
        .collect::<Vec<_>>();
    let mut keyed = rows
        .into_iter()
        .map(|row| {
            let keys = expressions
                .iter()
                .map(|expression| expression.evaluate(&row, context))
                .collect::<Result<Row>>()?;
            Ok((keys, row))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut error = None;
    keyed.sort_by(|(a, _), (b, _)| {
        if error.is_some() {
            return Ordering::Equal;
        }
        if let Err(e) = context.query.check() {
            error = Some(e);
            return Ordering::Equal;
        }
        for (((a, b), order), data_type) in a.iter().zip(b).zip(order).zip(&types) {
            let comparison = match (a.is_null(), b.is_null()) {
                (true, true) => Ordering::Equal,
                (true, false) => {
                    if order.nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (false, true) => {
                    if order.nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (false, false) => match data_type.compare(a, b, context.query) {
                    Ok(o) => {
                        if order.descending {
                            o.reverse()
                        } else {
                            o
                        }
                    }
                    Err(e) => {
                        error = Some(e);
                        return Ordering::Equal;
                    }
                },
            };
            if comparison != Ordering::Equal {
                return comparison;
            }
        }
        Ordering::Equal
    });
    if let Some(error) = error {
        return Err(error);
    }
    Ok(keyed.into_iter().map(|(_, row)| row).collect())
}
