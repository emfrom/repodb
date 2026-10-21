use rusqlite::ffi;
use std::ffi::{c_char, c_int};

type ExtensionInit = unsafe extern "C" fn(
    *mut ffi::sqlite3,
    *mut *mut c_char,
    *const ffi::sqlite3_api_routines,
) -> c_int;

fn entry_point() -> ExtensionInit {
    // SAFETY: sqlite-vec 0.1.9 exports `sqlite3_vec_init` with the SQLite extension ABI
    // declared in its bundled sqlite-vec.h. Its Rust binding declares the external symbol
    // without parameters, so this converts only the function-pointer type; SQLite calls it
    // later with the exact db/error-message/api-table parameters required by that C ABI.
    unsafe {
        std::mem::transmute::<*const (), ExtensionInit>(sqlite_vec::sqlite3_vec_init as *const ())
    }
}

pub(crate) fn register() -> rusqlite::Result<()> {
    // SAFETY: `entry_point` has the callback signature required by sqlite3_auto_extension,
    // and the linked C function remains valid for the lifetime of the process.
    let result = unsafe { ffi::sqlite3_auto_extension(Some(entry_point())) };
    if result == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(
            ffi::Error::new(result),
            None,
        ))
    }
}
