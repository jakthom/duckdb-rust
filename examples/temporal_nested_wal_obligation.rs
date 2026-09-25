//! Run the retained cross-family WAL obligation without marking it ignored or
//! counting it as an ordinary passing checkpoint case.
#[path = "../test/obligations/clock_domain.rs"]
mod clock_domain;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn main() -> duckdb_rust::Result<()> {
    clock_domain::run(true)
}
