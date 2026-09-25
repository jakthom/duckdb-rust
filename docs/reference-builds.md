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

## Interpreting outcomes

Use a fresh output directory and retain both source/executable identities for
every run. An early file-campaign failure leaves subsequent cases unexecuted.
Different ALTER behavior between pins must be recorded under the development-wins
rule, not hidden by changing assertions. Performance acceptance uses the faster
reference separately for each semantically comparable workload.

This runbook describes reproduction, not current pass status. See the
[parity backlog](parity-backlog.md) for remaining obligations. Historical campaigns
and their failed trials remain recoverable from Git at `20c8214` and earlier.
