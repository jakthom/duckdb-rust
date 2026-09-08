# Catalog, namespaces, and dependencies

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

`Catalog` is the abstraction used by binding and DDL; `DuckCatalog` implements native metadata. Entries represent schemas, tables, views, sequences, indexes, types, scalar/aggregate/table functions, macros and other named objects. Schema entries organize namespace lookup, and catalog search paths define name resolution.

`CatalogSet` provides versioned entry storage, while dependency management records relationships needed for validity and drop/alter behavior. A catalog mutation must participate in transaction semantics: immediately changing a global map without version/undo handling would violate visibility and rollback contracts.

Native table catalog entries bridge logical metadata to `DataTable`. Extension catalogs can implement different table lookup, scan, planning and transaction behavior. System-table functions expose catalog and runtime metadata through SQL; that introspection surface also underpins tests, profiling and extension tooling.

Sources: [catalog.hpp](../../../duckdb/src/include/duckdb/catalog/catalog.hpp), [catalog_set.cpp](../../../duckdb/src/catalog/catalog_set.cpp), [duck_catalog.cpp](../../../duckdb/src/catalog/duck_catalog.cpp), [catalog entries](../../../duckdb/src/catalog/catalog_entry/), [dependency manager](../../../duckdb/src/catalog/dependency_manager.cpp), [system functions](../../../duckdb/src/function/table/system/).

## Lookup and mutation interfaces

Binding requests a typed entry through `EntryLookupInfo`, qualified names, and `CatalogEntryRetriever`. The lookup contract includes catalog/schema qualification, the requested catalog object category, transaction visibility, and missing-entry behavior. The current header deprecates several overloads that pass schema/catalog separately in favor of folding qualification into lookup information. A caller should not duplicate search-path logic around these interfaces.

`CatalogSet::CreateEntry` accepts owned entry data and dependencies. `AlterEntry` and `DropEntry` take a catalog transaction; drop additionally expresses cascade behavior and special internal-entry handling. `GetEntry` returns a visible entry, while `GetEntryDetailed` carries richer lookup information. Scanning committed entries without a transaction is a different operation from a transaction-visible scan. `ScanWithConflictDetection` additionally reports conflicting head entries; choosing the wrong scan overload can hide a write conflict or expose the wrong metadata view.

Sources: [CatalogSet interface](../../../duckdb/src/include/duckdb/catalog/catalog_set.hpp), [lookup input](../../../duckdb/src/include/duckdb/catalog/entry_lookup_info.hpp), [catalog transaction](../../../duckdb/src/include/duckdb/catalog/catalog_transaction.hpp).

## Versioning and dependency lifecycle

Catalog entries participate in version chains and transaction undo. Creating or altering an object publishes a transaction-aware version, not an unconditional overwrite of a process-global dictionary. Existing readers can require older versions; rollback and cleanup therefore have separate responsibilities. Dependency tracking connects objects such as views, indexes, types, and their referenced objects so dropping or altering one can reject unsafe changes or perform the specified cascade.

Prepared statements must account for metadata changes rather than indefinitely dereference an obsolete table definition. The catalog exposes version information through `GetCatalogVersion`; extension implementations may return no version. Version checking, bound-object dependencies, and rebind behavior must be considered together. A universal invalidation rule cannot be inferred from one native catalog counter alone.

## Native versus extension catalogs

`DuckCatalog` bridges to native schemas, table entries, storage, and transaction management. The abstract `Catalog` also offers physical-planning hooks for CREATE TABLE AS, INSERT, DELETE, UPDATE, and MERGE. Consequently, a bound table does not imply that the physical plan will use native `DataTable` mutation. The catalog's type and capabilities define which native assumptions are valid.

Catalog introspection is a public SQL surface backed by system table functions. Its output can include metadata used by clients and tests, but it is not a serialization of private pointers or an authorization boundary by itself.

## Verification requirements

Test qualification/search paths, missing and ambiguous entries, transactional DDL rollback, concurrent conflicting DDL, dependencies/cascade, prepared execution after schema change, and attachment lifecycle. Cross-check catalog introspection with successful SQL use of the reported objects. Extension catalogs additionally need tests that their own scan and mutation plans are selected. See [planner](planner.md), [transactions](transactions.md), [extensions](extensions.md), and [component/API harnesses](../testing/component-api.md).
