# Pinned C++ references

Both references are required and reported separately. The default compatibility
target is the v1.5.5 source build. Selecting development is explicit; there is no
fallback to another installed release. The Rust engine never links to these
reference libraries or invokes their CLIs.

| Target | Version and commit | Source / build, relative to the Rust repository |
| --- | --- | --- |
| `release` | v1.5.5, `d8cdaa33fda8df955cc76ef58a280f68f4cd43fa` | `../duckdb-v1.5.5`, `build/rewrite-reference` |
| `development` | v2.0.0-dev84019, `99063af2bd7092aff02e14184a20e24699d34d71` | `../duckdb`, `build/engine-walkthrough` |

[`reference_version.py`](../scripts/reference_version.py) owns target selection,
exact version/revision checks, resolved executable paths and binary hashes.
Source-build measurements also reject tracked source changes. Each campaign
prints its identity before running SQL. An explicit `--duckdb /path/to/duckdb`
may select another build of the same pinned release/commit; its actual identity
is still checked and recorded.

## Building v1.5.5

The detached worktree is separate from the original source checkout. Run these
commands from `duckdb-rust/` when provisioning a new workspace:

```sh
git -C ../duckdb worktree add --detach ../duckdb-v1.5.5 d8cdaa33fda8df955cc76ef58a280f68f4cd43fa
cmake -S ../duckdb-v1.5.5 -B ../duckdb-v1.5.5/build/rewrite-reference -G Ninja \
  -DCMAKE_BUILD_TYPE=Release '-DCMAKE_CXX_FLAGS_RELEASE=-O3 -DNDEBUG' \
  -DCMAKE_EXPORT_COMPILE_COMMANDS=ON -DBUILD_UNITTESTS=ON -DBUILD_SHELL=ON \
  '-DBUILD_EXTENSIONS=core_functions;parquet;json' \
  -DENABLE_EXTENSION_AUTOINSTALL=OFF -DENABLE_EXTENSION_AUTOLOADING=OFF \
  -DENABLE_SANITIZER=OFF -DENABLE_UBSAN=OFF
cmake --build ../duckdb-v1.5.5/build/rewrite-reference --target shell unittest --parallel 4
cmake --build ../duckdb-v1.5.5/build/rewrite-reference --target libduckdb.dylib --parallel 4
../duckdb-v1.5.5/build/rewrite-reference/duckdb --version
```

The shared-library target above is for macOS; Linux uses `libduckdb.so`. Under
Ninja, the output named `duckdb` selects the CLI, so explicitly build the library
as well. The verified macOS build reports `v1.5.5 (Variegata) d8cdaa33fd` and links
core_functions, Parquet and JSON. Automatic extension installation/loading is off.

The existing development build is preserved with its original configuration and
library hash. It links core_functions and Parquet, has automatic extension
installation/loading disabled, and was built with `BUILD_UNITTESTS=OFF`. It
therefore does not supply a native test runner. The generated compiler commands
end with `-O3` for both references. Build settings, cache hashes, available
artifacts and loaded/installed extension lists appear in the combined report.

## Running both references

First build the Rust release CLI. Stop other builds and performance campaigns
before measuring. The combined driver runs targets and measurements sequentially:

```sh
cargo build --offline --release --bin duckdb-rust
python3 scripts/check_references.py --output-dir target/reference-campaign --iterations 21
```

The destination must be new. Each target gets a file/query compatibility report,
an unchanged local ALTER corpus report, and a performance report with all samples.
Logs, hashes, commands, build identities and aggregate status are retained in
`summary.json`. One failed campaign does not prevent the other campaigns from
running. Missing builds/reports, incorrect results or ratios above 1.0 fail the
combined gate. Full compatibility/test/performance flags remain false because
these campaigns cover only their documented subsets.

Individual commands remain available:

```sh
python3 scripts/verify_reference.py --target release --report target/release-files.json
python3 scripts/verify_reference.py --target development --report target/development-files.json
python3 scripts/alter_reference.py --target release --report target/release-alter.json
python3 scripts/compare_native.py --target release --iterations 21 --report target/release-performance.json
python3 scripts/compare_native.py --target development --iterations 21 --report target/development-performance.json
```

The source archive and Rust upstream runner still use the original development
revision and retain its unchanged assertions. Compiling a C++ native runner is
not a Rust assertion mapping. The release runner's verified smoke selection
passes 19 assertions in two upstream ALTER tests; its registry lists 5,502 case
names. Neither observation establishes full native or Rust test parity.

Fixture generators also require a selected version/revision and an explicit
output directory. For example:

```sh
python3 scripts/generate_compatibility_fixtures.py --target release --case scalar --output-dir target/release-fixtures
python3 scripts/generate_wal_fixtures.py --target release --output-dir target/release-wal-fixtures
python3 scripts/generate_checkpoint_fixtures.py --target release --output-dir target/release-checkpoint-fixtures
```

Historical v1.3.0 fixtures retain their producer metadata. New generation does
not overwrite them by default. The user-installed v1.5.5 CLI is an additional
available binary; the default campaigns above use the source build explicitly.

## Recorded outcomes

The [latest performance comparisons](aggregation-exists/README.md) pass all
twelve workloads against both references with 21 paired samples and the
unchanged 1.0 maximum. Aggregation and correlated EXISTS take 0.737× and 0.181×
the v1.5.5 times; development ratios are 0.243× and 0.152×. The code review,
implementation changes, tests, profiles, plans and failed intermediate
measurements are retained with those reports.

The earlier combined source-build campaign
remains **failing**, with unresolved file/ALTER compatibility gaps:

| Check | v1.5.5 | Development |
| --- | --- | --- |
| File/query campaign | 28 checks completed, then Rust rejects native WAL metadata field 103 | Reference fails opening a historical checkpoint before completing a check |
| Local ALTER corpus, checkpoint and WAL configurations | Reference accepts dropping NOT NULL on a primary-key column; the unchanged negative assertion fails | Reference accepts ADD COLUMN NOT NULL while discarding the constraint; the unchanged negative assertion fails |
| Performance, 21 paired samples | 10/12 pass; aggregation 1.196× C++, correlated EXISTS 1.400× C++ | 12/12 pass |

Checks after a file-campaign error remain unexecuted, not passed. These runs do
not establish general file compatibility, full ALTER parity or complete
performance parity. Its failed performance measurements are preserved; the
latest comparisons above resolve those two measured regressions for the
covered workloads.

The initial setup attempt is retained: the
v1.5.5 CLI and native runner had built, but the shared library had not yet been
linked. Its development campaigns ran; its release campaigns did not. The
subsequent campaign above starts after explicit library linking and contains all
six report identities. No failed report was overwritten or relabeled.
