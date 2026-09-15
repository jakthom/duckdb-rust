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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::CStr, os::raw::c_void};

    unsafe extern "C" {
        fn free(ptr: *mut c_void);
    }

    #[test]
    fn pinned_property_layout_preserves_full_combination_index() {
        assert_eq!(std::mem::size_of::<utf8proc_property_t>(), 24);
        assert_eq!(std::mem::offset_of!(utf8proc_property_t, comb_index), 18);
        assert_eq!(std::mem::offset_of!(utf8proc_property_t, _bitfield_1), 20);

        let property = unsafe { &*utf8proc_get_property('B' as utf8proc_int32_t) };
        assert_eq!(property.comb_index, 2784);
        assert_eq!(property.charwidth(), 1);
    }

    #[test]
    fn bindings_expose_only_symbols_from_the_pinned_header() {
        let input = b"Caf\xC3\xA9";
        let output = unsafe {
            utf8proc_remove_accents(input.as_ptr(), input.len() as utf8proc_ssize_t)
        };
        assert!(!output.is_null());
        let result = unsafe { CStr::from_ptr(output.cast()) };
        assert_eq!(result.to_bytes(), b"Cafe");
        unsafe { free(output.cast()) };
    }
}
