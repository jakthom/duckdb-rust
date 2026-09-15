#[path = "runner/mod.rs"]
mod runner;

use duckdb_rust::Database;
use flate2::{Compression, write::GzEncoder};
use std::io::Write;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("test")).unwrap();
    root
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integrated_parser_fixture_oracle_matches_source_controls() -> duckdb_rust::Result<()> {
    let root = root();
    std::fs::write(
        root.path().join("test/child.test"),
        "query I\nSELECT 41\n----\n41\n\n",
    )?;
    std::fs::write(root.path().join("expected.csv"), "answer|text\n42|needle\n")?;
    let gzip = std::fs::File::create(root.path().join("fixture.txt.gz"))?;
    let mut gzip = GzEncoder::new(gzip, Compression::default());
    gzip.write_all(b"source fixture")?;
    gzip.finish()?;
    let source = "include test/child.test\n\n\
                  unzip fixture.txt.gz {TEST_DIR}/extracted.txt\n\n\
                  unzip fixture.txt.gz {TEST_DIR}/extracted.txt\n\n\
                  unzip fixture.txt.gz NULL\n\n\
                  unzip fixture.txt.gz NULL\n\n\
                  query II rowsort\nSELECT 42, 'needle'\n----\n<FILE>:expected.csv\n\n\
                  query I\nSELECT 'needle'\n----\n<REGEX>:.*needle.*\n\n\
                  query I\nSELECT 'absent'\n----\n<!REGEX>:.*needle.*\n\n\
                  query I nosort stable\nSELECT 7\n----\nignored\n\n\
                  query I nosort stable\nSELECT 7\n----\nalso ignored\n";
    let path = root.path().join("test/root.test");
    std::fs::write(&path, source)?;
    let report = runner::run_file_report(&Database::memory()?, &path)?;
    assert_eq!(report.status, runner::FileStatus::Passed);
    assert_eq!(
        (report.declarations, report.passed, report.skipped),
        (6, 6, 0)
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn invalid_utf8_transport_limitation_and_skip_are_reported_honestly() -> duckdb_rust::Result<()> {
    let root = root();
    let invalid = root.path().join("test/invalid.test");
    let mut source = b"statement error\nSELECT 1 ".to_vec();
    source.push(0xf9);
    source.extend_from_slice(b"\n----\nInvalid UTF-8 in query\n");
    std::fs::write(&invalid, source)?;
    let error = runner::run_file_report(&Database::memory()?, &invalid).unwrap_err();
    assert!(
        error.to_string().contains("engine was not invoked"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("SQLLogic transport accepts only UTF-8 SQL"),
        "{error}"
    );

    let skipped = root.path().join("test/skipped.test");
    std::fs::write(
        &skipped,
        "skipif duckdb\nquery I\nSELECT 1\n----\n1\n\n\
         query I\nSELECT 'text'\n----\ntext\n",
    )?;
    let report = runner::run_file_report(&Database::memory()?, &skipped)?;
    assert!(matches!(report.status, runner::FileStatus::Skipped(_)));
    assert_eq!(
        (report.declarations, report.passed, report.skipped),
        (2, 1, 1)
    );
    assert!(runner::run_file(&Database::memory()?, &skipped).is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn mode_skip_suppresses_all_non_mode_directives_and_debug_output_is_observable()
-> duckdb_rust::Result<()> {
    let root = root();
    let path = root.path().join("test/modes.test");
    std::fs::write(
        &path,
        "mode skip ignored controls\n\n\
         include missing.test\n\n\
         unzip missing.gz {TEST_DIR}/missing\n\n\
         require definitely_missing\n\n\
         set unsupported ignored\n\n\
         statement ok\nSELECT missing\n\n\
         mode output_result\n\n\
         mode unskip\n\n\
         statement debug_skip\nSELECT 42\n\n\
         hash-threshold not-a-number\n\n\
         query I\nSELECT 99\n----\nwrong while skipped\n\n\
         mode output_hash\n\n\
         mode unskip\n\n\
         query I\nSELECT 7\n----\n7\n\n\
         mode no_output\n\n\
         mode debug\n\n\
         statement ok\nSELECT 8\n",
    )?;
    let report = runner::run_file_report(&Database::memory()?, &path)?;
    assert!(matches!(report.status, runner::FileStatus::Skipped(_)));
    assert_eq!(
        (report.declarations, report.passed, report.skipped),
        (5, 3, 2)
    );
    assert!(report.output.iter().any(|line| line.contains("SELECT 42")));
    assert!(
        report
            .output
            .iter()
            .any(|line| line.contains("OUTPUT_HASH requested for 1 value"))
    );
    assert!(report.output.iter().any(|line| line.contains("SELECT 8")));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn output_hash_mode_does_not_hide_a_wrong_result() -> duckdb_rust::Result<()> {
    let root = root();
    let path = root.path().join("test/output_hash_wrong.test");
    std::fs::write(&path, "mode output_hash\n\nquery I\nSELECT 1\n----\n2\n")?;
    assert!(runner::run_file_report(&Database::memory()?, &path).is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn wrong_results_hashes_regexes_and_fixture_paths_fail_closed() -> duckdb_rust::Result<()> {
    let root = root();
    for (name, source) in [
        ("value", "query I\nSELECT 1\n----\n2\n"),
        (
            "label",
            "query I nosort same\nSELECT 1\n----\n1\n\nquery I nosort same\nSELECT 2\n----\n2\n",
        ),
        ("regex", "query I\nSELECT 'x'\n----\n<REGEX>:[\n"),
        ("file", "query I\nSELECT 1\n----\n<FILE>:../escape.csv\n"),
    ] {
        let path = root.path().join(format!("test/{name}.test"));
        std::fs::write(&path, source)?;
        assert!(
            runner::run_file(&Database::memory()?, &path).is_err(),
            "{name}"
        );
    }
    Ok(())
}
