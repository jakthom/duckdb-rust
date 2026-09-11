# Exact native checkpoint content

This is continuing integration work after pushed checkpoint `9e19237`, not a
claim of complete VARIANT recovery or value-and-expression parity.

The existing bounded native VARIANT comparison is now reachable through a
defaulted `SnapshotFormat::checkpoint_value_equivalent` hook. It returns no
decision for non-VARIANT values; formats that do not opt in retain exact physical
comparison. The shared validator still owns complete row mappings, uniqueness,
catalog metadata and append watermarks. Both recovery preparation and live WAL
rebasing pass the actual selected format before any publication/session change.

Declared column and child bindings come from the source snapshot's retained
registry. Defaults use the same comparison path as rows, even in empty tables;
no SQL is evaluated. Canonical VARIANT wrappers can differ, but native scalar
tags/widths, decimal metadata, floating bits, exact object names/order and child
NULL presence cannot. Outside VARIANT, nested metadata, union tags and all
physical values stay exact. This is not SQL equality or grouping-key equality.

The provisional borrowed traversal preflights every complete row on both sides
against the existing depth-64/16-million-visit bounds before delegating leaves.
It does not skip shared Arc subtrees. The native comparator keeps its additional
64 MiB variable scalar/key byte budget. This avoids an outer-row budget reset
at each VARIANT leaf without allocating a canonicalized tree. The extra traversal
has not received a new performance acceptance campaign.

Connected tests cover native encode/decode of VARIANT inside LIST, ARRAY,
STRUCT, TUPLE, MAP keys/values, UNION and another VARIANT; decimals and nanosecond
timestamps inside the dynamic value; exact defaults with and without rows;
changed tags/widths, zero signs/NaN payloads, object names/order/NULL presence;
malformed mappings; retained replacement validation under an empty ambient
registry; and selected errors/cancellation. Separate rejection points exercise
both recovery preparation and live-session rebasing, retaining checkpoint/WAL
bytes and continuing mutations/reopen afterward.

Non-NULL nested DEFAULT native encoding is still unsupported. Default comparison
tests deliberately use independently constructed snapshots, not a claimed native
DEFAULT round trip. The WAL version/capability and VARIANT codec gates remain
closed while their separate implementations and integrated checks proceed.
Substantial combined validation/Kani results will be recorded after integration;
the prior ninth-checkpoint timing and upstream measurements do not cover this
new source.

The focused prerequisite pass runs ordinary workspace check, all 61 library
tests, all 17 checkpoint tests and all-target clippy with warnings denied.
Coverage finds no missing instrumentation across 330 files, 3,080 functions and
217 interface methods. Traced all-target compilation passes in 48.744 seconds,
with no errors, panics or open spans; temporary telemetry is deleted. These
results precede the next combined workspace/exploratory Kani checkpoint.
