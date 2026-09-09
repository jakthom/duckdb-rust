use duckdb_rust::{DatabaseBuilder, Result, execution::StreamControl};

fn main() -> Result<()> {
    let mut connection = DatabaseBuilder::new()
        .batch_size(128)
        .max_intermediate_rows(128)
        .build()?
        .connect();
    let mut sum = 0;
    let summary =
        connection.query_batches("SELECT range AS i FROM range(10000)", |columns, batch| {
            assert_eq!(columns[0].name, "i");
            for row in batch.rows() {
                sum += row[0].as_i128()?;
            }
            Ok(StreamControl::Continue)
        })?;
    assert_eq!(sum, 49_995_000);
    assert_eq!(summary.execution.rows_delivered, 10000);
    Ok(())
}
