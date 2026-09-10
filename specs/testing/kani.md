# Kani stage validation

[Rewrite principles](../rewrite-principles.md) · [Parity acceptance](parity.md) · [Implementation checks](../../docs/verification.md)

Accepted rewrite requirement, 2026-09-10. [Kani](https://github.com/model-checking/kani)
is a required checkpoint for each substantial Rust implementation chunk. This is
a rewrite policy, separate from the C++ source-baseline harness inventory.

## Cadence and completion

Use normal checks, unit/contract tests, and targeted reproductions while editing.
Run Kani before completing a feature slice, subsystem or adapter, cross-module
refactor, or change to a significant representation, ownership, arithmetic, or
state-transition invariant. A chunk can span multiple commits. Every such chunk
gets a checkpoint, even within a long-running PR; do not wait for final PR review.
Documentation-only, formatting, and isolated low-impact edits need no Kani run.

At the checkpoint:

1. Identify changed invariants and add or update proofs for the tractable Rust
   logic. Exercise the production functions and relevant adapter contracts.
2. Run `python3 scripts/verify_kani.py` from the rewrite checkout. This runs all
   maintained library harnesses with tracing disabled, including earlier proofs.
3. Resolve failures and rerun. Timeouts, unsupported reachable code, insufficient
   unwind bounds, compilation/setup failures, and zero verified harnesses leave
   the gate incomplete. An unexecuted or failed run cannot be reported as passed.
4. Record the stage, source revision and local changes, Kani version, command,
   harness names, input assumptions, bounds, results, and remaining gaps in the
   stage validation summary. Run the ordinary, compatibility, and performance
   checks required by that change as well.

For code that Kani cannot handle directly, isolate a bounded contract or pure
transition while keeping its connection to production explicit. Document any
model/stub and the behavior it omits. Do not replace production with a simplified
copy solely to obtain a green result. If the changed behavior remains outside
proof scope, record that gap even when the existing suite passes.

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
minutes. A timeout is a failure to complete validation, not a waived proof.
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
must grow with the implementation; a fixed initial suite alone does not cover
each future feature.

The [initial integration record](../../docs/kani-validation.json) contains the
executed results and checked source hashes, including the independently
reproduced SQL-suite failure on the unmodified PR base.
