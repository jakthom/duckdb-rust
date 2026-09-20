use duckdb_rust::{DatabaseBuilder, Error, Result};
use std::io::Write;
fn main() -> Result<()> {
 let args:Vec<_>=std::env::args().skip(1).collect(); if args.len()!=3{return Err(Error::Parse("path rows options".into()))}
 let path=&args[0]; let rows:usize=args[1].parse().map_err(|_|Error::Parse("rows".into()))?; let options=&args[2];
 let mut c=DatabaseBuilder::new().build()?.connect(); c.execute("CREATE TABLE f2(i INTEGER, note VARCHAR)")?;
 for i in 0..rows { c.execute(&format!("INSERT INTO f2 VALUES ({i}, 'row,{i} \"quoted\"')"))?; }
 let started=std::time::Instant::now(); let result=c.query(&format!("COPY f2 TO '{}'{}",path.replace('\'',"''"),options))?;
 let bytes=std::fs::read(path)?; let elapsed=started.elapsed().as_nanos(); let hash=bytes.iter().fold(0u64,|a,b|a.wrapping_mul(257).wrapping_add(u64::from(*b)));
 println!("{{\"engine\":\"rust\",\"rows\":{},\"written\":{},\"bytes\":{},\"elapsed_ns\":{},\"hash\":{}}}",rows,result.affected_rows,bytes.len(),elapsed,hash); std::io::stdout().flush()?; Ok(())
}
