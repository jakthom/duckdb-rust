# utf8proc provenance

This API-compatible `utf8proc-sys` vendor copy is derived from the utf8proc
2.9.0 sources embedded by both pinned DuckDB references:

- development: `third_party/utf8proc` at `99063af2bd7092aff02e14184a20e24699d34d71`
- release: `third_party/utf8proc` at `d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`

`utf8proc/utf8proc_data.c` is copied byte-for-byte from each reference's
`utf8proc_data.cpp`; SHA-256:
`8d4e9275064306187611cc3d54d81ef1d2937d3ce185ea8097343ded0998c961`.

The pinned implementation advertises Unicode 15.1.0. The small C adaptation
removes DuckDB's C++ namespace, converts two `nullptr` expressions, and restores
the upstream null-terminated ABI for the five NFD/NFC convenience exports used
by the retained Rust bindings; the data table is unmodified. Licensing is
retained in `LICENSE`.
