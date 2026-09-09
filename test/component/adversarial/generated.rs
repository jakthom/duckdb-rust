use duckdb_rust::{
    Database, DatabaseBuilder, Error, Result, Value,
    execution::{
        Executor, MaterializingExecutor, PullExecutor,
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        physical_plan::{NativePhysicalPlanner, ScanFilterStrategy},
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};
use std::{fs, sync::Arc};

fn next(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed
}

#[test]
fn generated_predicate_partitions_match_an_independent_row_model_across_adapters() -> Result<()> {
    for optimized in [false, true] {
        for fused in [ScanFilterStrategy::Separate, ScanFilterStrategy::Fused] {
            for eager in [false, true] {
                let optimizer: Arc<dyn Optimizer> = if optimized {
                    Arc::new(PipelineOptimizer::default())
                } else {
                    Arc::new(IdentityOptimizer)
                };
                let executor: Arc<dyn Executor> = if eager {
                    Arc::new(MaterializingExecutor)
                } else {
                    Arc::new(PullExecutor)
                };
                let indexes: Arc<dyn IndexFactory> = if eager {
                    Arc::new(BTreeIndexFactory)
                } else {
                    Arc::new(HashIndexFactory)
                };
                let db = DatabaseBuilder::new()
                    .optimizer(optimizer)
                    .executor(executor)
                    .indexes(indexes)
                    .physical_planner(Arc::new(
                        NativePhysicalPlanner::default().with_scan_filters(fused),
                    ))
                    .batch_size(7)
                    .build()?;
                let mut connection = db.connect();
                for seed in 0..8 {
                    let mut state = seed;
                    let mut model = Vec::new();
                    connection.execute(
                        "DROP TABLE IF EXISTS t; CREATE TABLE t(i INTEGER PRIMARY KEY,x INTEGER)",
                    )?;
                    for i in 0..32 {
                        let value = if next(&mut state).is_multiple_of(5) {
                            None
                        } else {
                            Some((next(&mut state) % 33) as i128 - 16)
                        };
                        connection.execute(&format!(
                            "INSERT INTO t VALUES({i},{})",
                            value.map_or("NULL".into(), |v| v.to_string())
                        ))?;
                        model.push(vec![
                            Value::Integer(i),
                            value.map_or(Value::Null, Value::Integer),
                        ]);
                    }
                    for bound in [-16, -1, 0, 1, 16] {
                        let predicate = format!("x<{bound}");
                        let expected = model
                            .iter()
                            .filter(|row| matches!(row[1],Value::Integer(x) if x<bound))
                            .cloned()
                            .collect::<Vec<_>>();
                        assert_eq!(
                            connection
                                .query(&format!("SELECT i,x FROM t WHERE {predicate} ORDER BY i"))?
                                .rows,
                            expected
                        );
                        assert_eq!(connection.query(&format!("SELECT i,x FROM t WHERE {predicate} UNION ALL SELECT i,x FROM t WHERE NOT({predicate}) UNION ALL SELECT i,x FROM t WHERE ({predicate}) IS NULL ORDER BY i"))?.rows,model);
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn paired_sql_and_checkpoint_mutations_return_results_or_checked_errors() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let original = directory.path().join("original.duckdb");
    Database::open(&original)?.connect().execute(
        "CREATE TABLE t(i INTEGER,v VARCHAR); INSERT INTO t VALUES(1,'a'),(2,'b'),(NULL,NULL)",
    )?;
    let bytes = fs::read(&original)?;
    let target = directory.path().join("mutated.duckdb");
    let queries = [
        "SELECT count(*) FROM t",
        "SELECT i FROM t ORDER BY i",
        "SELECT * FROM t WHERE i IN(SELECT i FROM t)",
        "SELECT CAST(v AS BIGINT) FROM t",
        "SELECT CASE WHEN i IS NULL THEN 7 ELSE i END FROM t",
    ];
    for seed in 0..128_u64 {
        let mut state = seed;
        let mut input = bytes.clone();
        let block = (next(&mut state) as usize) % ((bytes.len() - 12288) / 262144);
        let start = 12288 + block * 262144;
        let offset = start + 8 + (next(&mut state) as usize) % 2048;
        input[offset] ^= 1 << (next(&mut state) % 8);
        if seed % 2 == 0 {
            // Re-sign the affected block to reach structural checks behind its checksum.
            let mut checksum = 5381_u64;
            for word in input[start + 8..start + 262144].chunks_exact(8) {
                checksum ^=
                    u64::from_le_bytes(word.try_into().unwrap()).wrapping_mul(0xbf58476d1ce4e5b9);
            }
            input[start..start + 8].copy_from_slice(&checksum.to_le_bytes());
        }
        fs::write(&target, &input)?;
        let sql = queries[(next(&mut state) as usize) % queries.len()];
        let result = std::panic::catch_unwind(|| {
            let db = Database::open_read_only(&target)?;
            let mut connection = db.connect();
            connection.set_timeout(Some(std::time::Duration::from_millis(100)));
            connection.query(sql)
        });
        if matches!(&result, Err(_) | Ok(Err(Error::Internal(_)))) {
            let failure =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/generated-failures");
            fs::create_dir_all(&failure)?;
            fs::write(failure.join(format!("checkpoint-{seed}.duckdb")), &input)?;
            fs::write(failure.join(format!("checkpoint-{seed}.sql")), sql)?;
            panic!("seed {seed} produced a panic or internal error: {result:?}");
        }
        assert_eq!(
            fs::read(&target)?,
            input,
            "read-only mutation changed the input"
        );
    }
    Ok(())
}
