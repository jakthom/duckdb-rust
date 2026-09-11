//! Physical input metadata borrowed only for the current execution scope.
use super::*;
use crate::common::{
    DataType,
    vector::{DataChunk, Vector},
};

pub(crate) struct BatchContext<'a> {
    pub(crate) parent: &'a dyn EvaluationContext,
    pub(crate) input: &'a DataChunk,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl EvaluationContext for BatchContext<'_> {
    fn query(&self) -> &QueryContext {
        self.parent.query()
    }
    fn column_provenance(&self, column: usize) -> ArgumentProvenance {
        if self
            .input
            .columns()
            .get(column)
            .and_then(Vector::constant_value)
            .is_some()
        {
            ArgumentProvenance::Constant
        } else {
            ArgumentProvenance::Unknown
        }
    }
    fn outer_column(&self, depth: usize, column: usize) -> Result<Value> {
        self.parent.outer_column(depth, column)
    }
    fn prepared_subquery(
        &self,
        query: &std::sync::Arc<crate::planner::expression::BoundSubquery>,
    ) -> Option<Value> {
        self.parent.prepared_subquery(query)
    }
    fn subquery_provenance(
        &self,
        query: &std::sync::Arc<crate::planner::expression::BoundSubquery>,
    ) -> ArgumentProvenance {
        self.parent.subquery_provenance(query)
    }
    fn subquery(
        &self,
        query: &std::sync::Arc<crate::planner::expression::BoundSubquery>,
        request: SubqueryRequest<'_>,
        row: &Row,
    ) -> Result<Value> {
        self.parent.subquery(query, request, row)
    }
}

/// Keep the selected evaluator's encoding assertion. Never infer Constant by
/// equality or cardinality; an empty result has no evaluated value to repeat.
/// All requested rows were evaluated before this representation choice.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn result_column(
    data_type: DataType,
    values: Vec<EvaluatedValue>,
    query: &QueryContext,
) -> Result<Vector> {
    query.check()?;
    let constant = !values.is_empty()
        && values
            .iter()
            .all(|result| result.provenance == ArgumentProvenance::Constant);
    let flat = Vector::flat(
        data_type.clone(),
        values.into_iter().map(|result| result.value).collect(),
    )?;
    if constant {
        // Metadata never permits discarding malformed output from later rows.
        // As with adapter effect/totality declarations, batch invariance is an
        // explicit selected-evaluator contract, not inferred value equality.
        query
            .types()
            .bind(&data_type)?
            .validate_vector(&flat, query)
            .map_err(|error| match error {
                Error::Conversion(_) => Error::Internal(
                    "expression evaluator returned an invalid logical constant value".into(),
                ),
                other => other,
            })?;
        return Vector::constant(
            data_type,
            flat.get(0).expect("nonempty constant output").clone(),
            flat.len(),
        );
    }
    Ok(flat)
}
