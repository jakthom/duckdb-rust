use super::*;
use crate::{
    common::{
        DataType, Error, Value,
        type_registry::{TypeRegistry, ascii},
    },
    execution::{
        expression_executor::ScalarEvaluator,
        physical_plan::NativePhysicalPlanner,
        subquery::{PreparedSubqueries, StreamingSubqueries},
    },
    parallel::QueryContext,
    planner::{ExprKind, Field},
    storage::{checkpoint::MemoryDurability, scan::ScanBatch},
    transaction::{SnapshotTransactions, TransactionManager},
};
use std::sync::Arc;

struct SuppliedRows(Option<ScanBatch>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableScan for SuppliedRows {
    fn next(&mut self, _: usize, _: &QueryContext) -> Result<Option<ScanBatch>> {
        Ok(self.0.take())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn rejected_scan_rows_still_require_valid_width_physical_and_logical_types() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    types.register(ascii::FAMILY, Arc::new(ascii::StreamingAscii))?;
    let ascii = ascii::data_type(1)?;
    let query = QueryContext::background().with_types(Arc::new(types));
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let transaction = manager.begin()?;
    let planner = NativePhysicalPlanner::default();
    let prepared = PreparedSubqueries::new(&planner);
    let context = ExecutionContext {
        transaction: transaction.as_ref(),
        query: &query,
        expressions: &ScalarEvaluator,
        subquery_plans: &prepared,
        subqueries: &StreamingSubqueries,
        outer: None,
        recursive: None,
    };
    let predicate = BoundExpr {
        kind: ExprKind::Literal(Value::Boolean(false)),
        data_type: DataType::Boolean,
    };
    for (data_type, supplied_types, row) in [
        (DataType::Integer, vec![], vec![]),
        (
            DataType::Integer,
            vec![DataType::Varchar],
            vec![Value::Varchar("bad".into())],
        ),
        (
            ascii.clone(),
            vec![ascii.clone()],
            vec![Value::extension(ascii, b"too long".to_vec())],
        ),
    ] {
        let schema = vec![Field::new("v", data_type)];
        let batch = crate::common::vector::DataChunk::from_rows(&supplied_types, &[row])?;
        let scan = Box::new(SuppliedRows(Some(ScanBatch::new(vec![0], batch)?)));
        let mut stream = filtered(scan, &schema, &predicate, &context)?;
        assert!(matches!(stream.next(1), Err(Error::Internal(_))));
        assert!(stream.next(1)?.is_none(), "failed cursor is terminal");
    }
    Ok(())
}
