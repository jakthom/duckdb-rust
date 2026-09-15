#![deny(missing_abi)]
//! Unsafe native bindings to the [utf8proc] library.
//!
//! **WARNING**: Right now this crate only supports static linking.
//!
//! [utf8proc]: https://juliastrings.github.io/utf8proc/

#[allow(
// docs are not in rust format
    rustdoc::bare_urls,
// names are not in a rust style
    bad_style,
// don't do clippy lints for generated files
    clippy::pedantic,
    clippy::style,
    clippy::complexity,
    clippy::restriction,
)]
mod generated;
pub use generated::*;

/// Memory could not be allocated.
pub const UTF8PROC_ERROR_NOMEM: utf8proc_ssize_t = -1;
/// The given string is too long to be processed.
pub const UTF8PROC_ERROR_OVERFLOW: utf8proc_ssize_t = -2;
/// The given string is not a legal UTF-8 string.
pub const UTF8PROC_ERROR_INVALIDUTF8: utf8proc_ssize_t = -3;
/// The `UTF8PROC_REJECTNA` flag was set and an unassigned codepoint was found. */
pub const UTF8PROC_ERROR_NOTASSIGNED: utf8proc_ssize_t = -4;
/// Invalid options have been used.
pub const UTF8PROC_ERROR_INVALIDOPTS: utf8proc_ssize_t = -5;

impl utf8proc_option_t {
    /// Indicates no options are set.
    pub const NONE: utf8proc_option_t = utf8proc_option_t(0);
}
