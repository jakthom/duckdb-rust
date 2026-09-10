use super::*;
use std::collections::BTreeMap;

#[test]
fn integer_grouping_matches_an_independent_cube_across_dense_sparse_and_null_domains() -> Result<()>
{
    let rows = (0..600)
        .map(|i| {
            let a = if i > 590 {
                Some(if i % 2 == 0 { i128::MIN } else { i128::MAX })
            } else if i % 11 == 0 {
                None
            } else {
                Some(i % 9 - 4)
            };
            let b = if i % 13 == 0 { None } else { Some(i % 3) };
            let value = if i % 5 == 0 {
                None
            } else {
                Some(if i % 2 == 0 {
                    i64::MAX as i128
                } else {
                    i64::MIN as i128
                })
            };
            (a, b, value)
        })
        .collect::<Vec<_>>();
    let mut model = BTreeMap::new();
    for &(a, b, value) in &rows {
        for mask in 0..4 {
            let key = (
                mask,
                if mask & 2 == 0 { a } else { None },
                if mask & 1 == 0 { b } else { None },
            );
            let (count, sum) = model.entry(key).or_insert((0_i128, None::<i128>));
            *count += 1;
            if let Some(value) = value {
                *sum = Some(sum.unwrap_or(0) + value);
            }
        }
    }
    let value = |v: Option<i128>| v.map_or(Value::Null, Value::Integer);
    let expected = model
        .into_iter()
        .map(|((mask, a, b), (count, sum))| {
            vec![
                value(a),
                value(b),
                value(sum),
                Value::Integer(count),
                Value::Integer(mask),
            ]
        })
        .collect::<Vec<_>>();
    let literal = |value: Option<i128>| value.map_or("NULL".to_string(), |v| v.to_string());
    let insert = format!(
        "INSERT INTO t VALUES {}",
        rows.iter()
            .map(|&(a, b, v)| format!("({},{},{})", literal(a), literal(b), literal(v)))
            .collect::<Vec<_>>()
            .join(",")
    );
    for algorithm in algorithms() {
        for size in [1, 7, 128, 2048] {
            let db = DatabaseBuilder::new()
                .batch_size(size)
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_aggregation(algorithm.clone()),
                ))
                .build()?;
            let mut c = db.connect();
            c.execute("CREATE TABLE t(a HUGEINT,b HUGEINT,v BIGINT)")?;
            c.execute(&insert)?;
            assert_eq!(c.query("SELECT a,b,sum(v),count(*),grouping(a,b) FROM t GROUP BY CUBE(a,b) ORDER BY grouping(a,b),a NULLS FIRST,b NULLS FIRST")?.rows, expected, "{} batch={size}",algorithm.name());
            assert!(matches!(c.query("SELECT sum(v),grouping(a) FROM (VALUES (1,170141183460469231731687303715884105727::HUGEINT),(1,1),(1,-1))t(a,v) GROUP BY ROLLUP(a)"), Err(Error::Execution(_))));
        }
    }
    Ok(())
}
