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
        (
            report.declarations,
            report.passed,
            report.skipped,
            report.generated
        ),
        (5, 1, 2, 2)
    );
    assert!(report.output.iter().any(|line| line.contains("SELECT 42")));
    assert!(
        report
            .output
            .iter()
            .any(|line| line == "1 values hashing to 84bc3da1b3e33a18e8d5e1bdd7a18d7a")
    );
    assert!(report.output.iter().any(|line| line.contains("SELECT 8")));
    assert!(report.output.iter().any(|line| line == "42"));
    assert!(report.output.iter().any(|line| line == "8"));
    assert!(
        !report
            .output
            .iter()
            .any(|line| line.contains("result set(s)"))
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn output_modes_emit_pinned_values_and_hash_without_comparing_generated_hash_expectations()
-> duckdb_rust::Result<()> {
    let root = root();
    let result_path = root.path().join("test/output_result.test");
    std::fs::write(
        &result_path,
        "mode output_result\n\nquery II\nSELECT true AS flag, '' AS blank\n----\n1\n(empty)\n",
    )?;
    let result_report = runner::run_file_report(&Database::memory()?, &result_path)?;
    assert_eq!(result_report.status, runner::FileStatus::Passed);
    assert_eq!(result_report.generated, 0);
    assert!(
        result_report
            .output
            .iter()
            .any(|line| line == "flag\tblank")
    );
    assert!(
        result_report
            .output
            .iter()
            .any(|line| line == "BOOLEAN\tVARCHAR")
    );
    assert!(result_report.output.iter().any(|line| line == "1\t(empty)"));

    let wrong_result_path = root.path().join("test/output_result_wrong.test");
    std::fs::write(
        &wrong_result_path,
        "mode output_result\n\nquery I\nSELECT 1\n----\n2\n",
    )?;
    assert!(runner::run_file_report(&Database::memory()?, &wrong_result_path).is_err());

    let path = root.path().join("test/output_hash_wrong.test");
    std::fs::write(&path, "mode output_hash\n\nquery I\nSELECT 1\n----\n2\n")?;
    let report = runner::run_file_report(&Database::memory()?, &path)?;
    assert!(matches!(
        report.status,
        runner::FileStatus::GeneratedOutput(_)
    ));
    assert_eq!((report.passed, report.generated), (0, 1));
    assert!(
        report
            .output
            .iter()
            .any(|line| line == "1 values hashing to b026324c6904b2a9cb4b88d6d61c81d1")
    );
    assert!(runner::run_file(&Database::memory()?, &path).is_err());

    let sorted_path = root.path().join("test/output_hash_sorted.test");
    std::fs::write(
        &sorted_path,
        "mode output_result\n\nmode output_hash\n\nquery I rowsort\nSELECT 2 AS i UNION ALL SELECT 1\n----\nignored\n",
    )?;
    let sorted = runner::run_file_report(&Database::memory()?, &sorted_path)?;
    assert!(matches!(
        sorted.status,
        runner::FileStatus::GeneratedOutput(_)
    ));
    assert!(
        sorted
            .output
            .iter()
            .any(|line| line == "2 values hashing to 6ddb4095eb719e2a9f0a3f95677d24e0")
    );
    let two = sorted.output.iter().position(|line| line == "2").unwrap();
    let one = sorted.output.iter().position(|line| line == "1").unwrap();
    assert!(
        two < one,
        "output_result must precede sorting: {:?}",
        sorted.output
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn re2_adapter_preserves_utf8_runes_and_ascii_perl_classes() -> duckdb_rust::Result<()> {
    let root = root();
    let passing = root.path().join("test/re2_utf8.test");
    std::fs::write(
        &passing,
        "query T\nSELECT 'é'\n----\n<REGEX>:.\n\n\
         query T\nSELECT 'éß'\n----\n<REGEX>:.{2}\n\n\
         query T\nSELECT 'é'\n----\n<!REGEX>:\\w\n\n\
         query T\nSELECT 'é'\n----\n<REGEX>:\\W\n\n\
         query T\nSELECT 'é'\n----\n<REGEX>:[\\W]\n\n\
         query T\nSELECT 'Az_09'\n----\n<REGEX>:\\w+\n\n\
         query T\nSELECT '١'\n----\n<!REGEX>:\\d\n\n\
         query T\nSELECT ' '\n----\n<!REGEX>:\\s\n\n\
         query T\nSELECT ' '\n----\n<REGEX>:\\s\n\n\
         query T\nSELECT '\u{11380}'\n----\n<!REGEX>:\\pL\n\n\
         query T\nSELECT '\u{1c8a}'\n----\n<!REGEX>:(?i)\\x{1C89}\n",
    )?;
    let report = runner::run_file_report(&Database::memory()?, &passing)?;
    assert_eq!(report.status, runner::FileStatus::Passed);
    assert_eq!(report.passed, 11);

    for (name, expected) in [
        ("negative_dot", "<!REGEX>:."),
        ("two_dots", "<REGEX>:.."),
        ("unicode_word", "<REGEX>:\\w"),
        ("negative_nonword", "<!REGEX>:\\W"),
    ] {
        let path = root.path().join(format!("test/{name}.test"));
        std::fs::write(&path, format!("query T\nSELECT 'é'\n----\n{expected}\n"))?;
        assert!(
            runner::run_file_report(&Database::memory()?, &path).is_err(),
            "{name} must reject the divergent expectation"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn re2_adapter_uses_union_only_character_classes() -> duckdb_rust::Result<()> {
    let root = root();
    let passing = root.path().join("test/re2_character_classes.test");
    std::fs::write(
        &passing,
        "query T\nSELECT 'a'\n----\n<REGEX>:[a&&b]\n\n\
         query T\nSELECT '&'\n----\n<REGEX>:[a&&b]\n\n\
         query T\nSELECT 'c'\n----\n<!REGEX>:[a&&b]\n\n\
         query T\nSELECT '~'\n----\n<REGEX>:[a~~b]\n\n\
         query T\nSELECT '0'\n----\n<REGEX>:[--a]\n\n\
         query T\nSELECT '/'\n----\n<REGEX>:[0-9--4]\n\n\
         query T\nSELECT '['\n----\n<REGEX>:[a[b]\n\n\
         query T\nSELECT ']'\n----\n<REGEX>:[]a]\n\n\
         query T\nSELECT 'A'\n----\n<REGEX>:[[:alpha:]]\n\n\
         query T\nSELECT 'é'\n----\n<!REGEX>:[\\w]\n",
    )?;
    let report = runner::run_file_report(&Database::memory()?, &passing)?;
    assert_eq!(report.status, runner::FileStatus::Passed);
    assert_eq!(report.passed, 10);

    for (name, value, expected) in [
        ("negative_literal_operator", "a", "<!REGEX>:[a&&b]"),
        ("positive_empty_intersection", "c", "<REGEX>:[a&&b]"),
        ("invalid_descending_range", "a", "<!REGEX>:[a--b]"),
    ] {
        let path = root.path().join(format!("test/{name}.test"));
        std::fs::write(
            &path,
            format!("query T\nSELECT '{value}'\n----\n{expected}\n"),
        )?;
        assert!(
            runner::run_file_report(&Database::memory()?, &path).is_err(),
            "{name} must reject the divergent expectation"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn re2_adapter_supports_byte_atoms_quotes_and_surrogate_classes() -> duckdb_rust::Result<()> {
    let root = root();
    let passing = root.path().join("test/re2_remaining_syntax.test");
    std::fs::write(
        &passing,
        "query T\nSELECT 'é'\n----\n<!REGEX>:\\C\n\n\
         query T\nSELECT 'é'\n----\n<REGEX>:\\C\\C\n\n\
         query T\nSELECT '.*[x](a)'\n----\n<REGEX>:\\Q.*[x](a)\\E\n\n\
         query T\nSELECT '[a&&b]'\n----\n<REGEX>:\\Q[a&&b]\\E\n\n\
         query T\nSELECT '\\'\n----\n<REGEX>:\\Q\\\\E\n\n\
         query T\nSELECT 'abc['\n----\n<REGEX>:\\Qabc[\n\n\
         query T\nSELECT 'a'\n----\n<!REGEX>:[\\x{D800}-\\x{DFFF}]\n\n\
         query T\nSELECT 'a'\n----\n<REGEX>:[^\\x{D800}-\\x{DFFF}]\n",
    )?;
    let report = runner::run_file_report(&Database::memory()?, &passing)?;
    assert_eq!(report.status, runner::FileStatus::Passed);
    assert_eq!(report.passed, 8);

    for (name, expected) in [
        ("positive_one_byte", "<REGEX>:\\C"),
        ("negative_two_bytes", "<!REGEX>:\\C\\C"),
        ("negative_quoted_literal", "<!REGEX>:\\Qé\\E"),
        ("positive_surrogate", "<REGEX>:[\\x{D800}]"),
    ] {
        let path = root.path().join(format!("test/{name}.test"));
        std::fs::write(&path, format!("query T\nSELECT 'é'\n----\n{expected}\n"))?;
        assert!(
            runner::run_file_report(&Database::memory()?, &path).is_err(),
            "{name} must reject the divergent expectation"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn cli_emits_file_report_generated_output() -> duckdb_rust::Result<()> {
    let root = root();
    let path = root.path().join("test/cli_output.test");
    std::fs::write(
        &path,
        "mode output_hash\n\nquery I\nSELECT 1\n----\nwrong\n",
    )?;
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_sqllogictest"))
        .arg(&path)
        .output()?;
    assert!(result.status.success());
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("1 values hashing to b026324c6904b2a9cb4b88d6d61c81d1"),
        "{stdout}"
    );
    assert!(stdout.contains("GENERATED "), "{stdout}");
    assert!(
        stdout.contains("0 records passed; 0 skipped; 1 generated"),
        "{stdout}"
    );

    let ordinary = root.path().join("test/cli_pass.test");
    std::fs::write(&ordinary, "query I\nSELECT 1\n----\n1\n")?;
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_sqllogictest"))
        .arg(&ordinary)
        .output()?;
    assert!(result.status.success());
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("PASS "), "{stdout}");
    assert!(stdout.contains("1 records passed; 0 skipped\n"), "{stdout}");
    assert!(!stdout.contains("generated"), "{stdout}");
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
