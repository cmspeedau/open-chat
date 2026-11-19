//! Dual-tier embedded memory (Roadmap Phase 1).
//!
//! Sparse tier: SQLite **FTS5** for exact/keyword recall. Dense tier:
//! **sqlite-vec** (`vec0` virtual table) for semantic kNN. [`MemoryStore::
//! recall`] fuses both with Reciprocal Rank Fusion under a token budget, so
//! the compositor gets a compact, deduplicated context slice.
//!
//! Contradiction resolution is upsert-on-key: storing a second entry with
//! the same `(scope, key)` overwrites the first, matching the README rule
//! that refactors "overwrite previous conventions rather than duplicating".

mod error;
mod store;

pub use error::MemoryError;
pub use store::{
    HitSource, MemoryEntry, MemoryScope, MemoryStore, RecallHit, estimate_tokens, vector_blob,
};

use std::sync::Once;

static VEC_INIT: Once = Once::new();

/// Register sqlite-vec with SQLite's auto-extension list so every new
/// connection (including test and in-memory ones) gains the `vec0` module.
///
/// Idempotent; call before opening any store. Safe to ignore the cost:
/// [`std::sync::Once`] gates it process-wide.
pub fn ensure_vec_extension() {
    VEC_INIT.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is the standard 3-arg extension init
        // entry point compiled with SQLITE_CORE against this exact bundled
        // libsqlite3; `sqlite3_auto_extension` stores it as an init callback
        // invoked per-connection (sqlite3 c3api: the casts are the
        // documented idiom for heterogeneous init signatures).
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}
