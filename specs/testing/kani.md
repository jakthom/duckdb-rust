# Kani exploratory checkpoints

[Rewrite principles](../rewrite-principles.md) · [Parity acceptance](parity.md) · [Chunk completion](../../docs/parity-backlog.md#how-to-assign-this-work)

Accepted rewrite requirement, 2026-09-10. [Kani](https://github.com/model-checking/kani)
is a required checkpoint for each substantial Rust implementation chunk. This is
a rewrite policy, separate from the C++ source-baseline harness inventory.

The current phase prioritizes design exploration. The requirement is to run the
tool, investigate findings, and report limits. A passing proof suite is not a
condition for completing a chunk. Formal proof coverage and acceptance gates
are deferred until the design settles.

## Cadence and completion

Use normal checks, unit/contract tests, and targeted reproductions while editing.
Run Kani before completing a feature slice, subsystem or adapter, cross-module
refactor, or change to a significant representation, ownership, arithmetic, or
state-transition invariant. A chunk can span multiple commits. Every such chunk
gets a checkpoint, even within a long-running PR; do not wait for final PR review.
Routine documentation, formatting and isolated low-impact edits do not create a
chunk boundary. At an explicitly declared work-chunk boundary, the full staged
sweep required by [AGENTS.md](../../AGENTS.md) includes the Kani checkpoint; this
applies even when that explicit chunk concerns documentation or tooling.

At the checkpoint:

1. Run `python3 scripts/verify_kani.py` from the rewrite checkout. This runs all
   maintained library harnesses with tracing disabled, including earlier proofs.
2. Investigate counterexamples against intended behavior. Distinguish an actual
   bug from an outdated assertion, incorrect harness assumption, or tool limit.
   Handle confirmed bugs through the ordinary correctness process; this policy
   does not make an incorrect implementation correct.
3. Record timeouts, unsupported code, insufficient unwind bounds, compilation or
   setup failures, and zero verified harnesses as unsuccessful or incomplete
   verification. Make a reasonable setup/retry attempt where practical, then
   continue exploration with unresolved limits explicit. These outcomes do not
   block chunk completion and must never be counted as successful proofs.
4. Briefly report the command, outcome, findings and limits in the chunk's
   summary. Link a reproducer or capture assumptions and bounds when needed to
   understand a finding. A new formal report artifact is not required for every
   checkpoint. Ordinary correctness, compatibility, and performance obligations
   remain applicable independently of Kani.

Add or adapt proofs when they help clarify a design that is taking shape. There
is no requirement to prove every changed invariant or increase proof coverage
with every chunk. Prefer observable behavior and intended contracts; internal
representations, algorithms, and module boundaries remain provisional. Revise or
retire an outdated harness with an explanation when an intended contract changes.
Do not remove a valid counterexample merely to obtain a passing result.

Kani's capabilities must not dictate the implementation. Do not restructure
production code or restrict algorithm, ownership, concurrency, or representation
choices solely to make them provable. If a small proof or model is useful, keep
its connection to production and its omissions explicit. Otherwise, record the
unproved scope and continue using other evidence while exploring the design.

## Installation and execution

The repository pins the verifier in [`.kani-version`](../../.kani-version).
From the rewrite root, install the pinned tool and its compiler bundle:

```sh
cargo install --locked kani-verifier --version "$(cat .kani-version)"
cargo kani setup
python3 scripts/verify_kani.py
```

Kani manages its compiler toolchain separately from ordinary Cargo builds. See
the official [installation guide](https://model-checking.github.io/kani/install-guide.html).
Update the pin deliberately and rerun the suite when changing it.

For proof development, select one harness, then use the full stage command when
the chunk is ready:

```sh
cargo kani -p duckdb-rust --lib --no-default-features \
  --harness kani_rows_frame_offsets_clip_to_partition
```

The stage runner checks the pinned version, preserves Kani's failure status,
rejects an empty/incomplete verification summary, and caps each harness at five
minutes. Its exit status describes the verification outcome, not whether an
exploratory chunk may be completed. Keep failures visible; do not suppress them
or turn this checkpoint into a mandatory passing CI/merge check during this phase.
It does not run automatically on every Cargo invocation or push.

## Harness conventions and interpretation

Place `#[kani::proof]` functions in `#[cfg(kani)]` modules beside the production
logic. The verifier supplies the `kani` crate; it is not a production dependency.
The package declares `cfg(kani)` for lint checking. Ordinary builds and tests
exclude the proof modules. See [Kani usage](https://model-checking.github.io/kani/usage.html).

Use symbolic inputs and independent assertions about observable results, errors,
or preserved state. `kani::assume` must express real caller preconditions or
documented proof-domain limits, never assume the desired result. Include invalid
input and boundary behavior where the interface must handle them.

Keep default safety, overflow, assertion-reachability, and unwinding checks
enabled. The package defaults to an unwind bound of one so unexpected loops
cannot run indefinitely. Loop-bearing harnesses must declare and justify a
sufficient `#[kani::unwind(N)]` and any input-size bound. Successful unwinding
checks establish adequacy for those inputs; they do not extend the input domain.
See [loop bounds](https://model-checking.github.io/kani/tutorial-loop-unwinding.html).

## Initial maintained proof scope

These harnesses call production code and cover arbitrary machine-width inputs
on the selected target. They do not enumerate database states or SQL programs.

| Harness | Property and domain |
| --- | --- |
| `kani_packed_byte_count_matches_wide_arithmetic` | All `usize` counts and widths; reject widths above 128 and unrepresentable padded byte sizes, otherwise match a `u128` size oracle. |
| `kani_uniform_window_bounds_validate_and_retain_range` | All `usize` ranges/counts; accept exactly ordered in-partition ranges, retain cardinality and range, and return it for any valid row index. Includes empty partitions. |
| `kani_rows_frame_offsets_clip_to_partition` | All `usize` partition sizes, valid row positions and offsets, both directions and endpoint kinds; ROWS offsets match a saturating unsigned oracle and stay inside the partition. Assumes only `index < count`. |

These are arithmetic and boundary proofs. Packed payload decoding, peer GROUPS
and RANGE semantics, full window results, concurrency, persistence and ecosystem
compatibility need their own verification. Passing Kani neither replaces the
upstream parity suite nor establishes zero performance regressions. Proof scope
can grow as useful contracts emerge; this initial suite does not cover each
future feature, and its existence does not freeze the current design.

The initial integration record contains the
executed results and checked source hashes, including the independently
reproduced SQL-suite failure on the unmodified PR base. It is historical evidence
from the original integration; its gate terminology predates this exploratory
policy and does not impose a current completion requirement.
