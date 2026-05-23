//! Registration helper for the sqlite-vec extension.
//!
//! Centralises the `unsafe transmute` so every callsite gets the same
//! typed signature (clippy demands an explicit type annotation on raw
//! transmutes; doing it once here is cleaner than four-way copy-paste).

use rusqlite::ffi;

/// Pointer signature SQLite expects for an extension init function.
type SqliteExtensionInit = unsafe extern "C" fn(
    *mut ffi::sqlite3,
    *mut *const i8,
    *const ffi::sqlite3_api_routines,
) -> i32;

/// Register sqlite-vec as an auto-extension. Every subsequent
/// `Connection::open*` picks it up, gaining access to the `vec0` virtual
/// table.
///
/// Calling this multiple times is safe — SQLite dedupes by function
/// pointer. The provider calls it on every `open` so callers don't have
/// to remember.
pub fn register() {
    // SAFETY: sqlite-vec's `sqlite3_vec_init` has the C signature SQLite
    // requires for an extension init function. The transmute reshapes the
    // crate's exported symbol (an `extern "C" fn` whose Rust type carries
    // no `unsafe`) into the type SQLite's FFI expects. SQLite stores the
    // pointer and invokes it on connection open.
    unsafe {
        let init_ptr: SqliteExtensionInit = std::mem::transmute::<*const (), SqliteExtensionInit>(
            sqlite_vec::sqlite3_vec_init as *const (),
        );
        ffi::sqlite3_auto_extension(Some(init_ptr));
    }
}
