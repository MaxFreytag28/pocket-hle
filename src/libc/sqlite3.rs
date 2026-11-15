/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `sqlite3.h` (a small subset of `libsqlite3.dylib`).
//!
//! This is backed by a real copy of SQLite running on the host. Databases are
//! always opened in memory: the guest file's content is loaded into the host
//! SQLite when the database is opened, and written back to the guest
//! filesystem after each statement that modifies it. This avoids needing a
//! SQLite VFS for the guest filesystem, and means read-only databases inside
//! an `.ipa` bundle work too.
//!
//! Handles (`sqlite3*`, `sqlite3_stmt*`) given to the guest are small dummy
//! guest allocations, used as keys to look up the host objects.

use crate::dyld::{export_c_func, FunctionExports, HostDylib};
use crate::fs::GuestPath;
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::Environment;
use libsqlite3_sys as ffi;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};

const SQLITE_OK: i32 = 0;
const SQLITE_ERROR: i32 = 1;
const SQLITE_MISUSE: i32 = 21;
const SQLITE_ROW: i32 = 100;
const SQLITE_DONE: i32 = 101;

struct DbHostObject {
    host: *mut ffi::sqlite3,
    /// Guest path the database should be written back to.
    path: String,
    /// Guest copy of the last string returned by `sqlite3_errmsg`.
    errmsg: Option<MutPtr<u8>>,
}

struct StmtHostObject {
    host: *mut ffi::sqlite3_stmt,
    db: MutVoidPtr,
    /// Guest copies of column values, valid until the next step/reset/finalize.
    row_allocs: Vec<MutVoidPtr>,
    /// Guest copies of column names, valid until finalize.
    name_allocs: HashMap<i32, MutPtr<u8>>,
}

#[derive(Default)]
pub struct State {
    dbs: HashMap<MutVoidPtr, DbHostObject>,
    stmts: HashMap<MutVoidPtr, StmtHostObject>,
}
impl State {
    fn get_mut(env: &mut Environment) -> &mut Self {
        &mut env.libc_state.sqlite3
    }
}

fn host_db(env: &mut Environment, db: MutVoidPtr) -> Option<*mut ffi::sqlite3> {
    State::get_mut(env).dbs.get(&db).map(|d| d.host)
}
fn host_stmt(env: &mut Environment, stmt: MutVoidPtr) -> Option<*mut ffi::sqlite3_stmt> {
    State::get_mut(env).stmts.get(&stmt).map(|s| s.host)
}

/// Write the current content of the database back to the guest filesystem.
fn flush_db(env: &mut Environment, db: MutVoidPtr) {
    let Some(db_obj) = State::get_mut(env).dbs.get(&db) else {
        return;
    };
    let host = db_obj.host;
    let path = db_obj.path.clone();
    let data = unsafe {
        let mut size: ffi::sqlite3_int64 = 0;
        let buf = ffi::sqlite3_serialize(host, c"main".as_ptr(), &mut size, 0);
        if buf.is_null() {
            // An empty database serializes to nothing.
            Vec::new()
        } else {
            let data = std::slice::from_raw_parts(buf, size as usize).to_vec();
            ffi::sqlite3_free(buf as *mut c_void);
            data
        }
    };
    if data.is_empty() && !env.fs.is_file(GuestPath::new(&path)) {
        return;
    }
    if env.fs.write(GuestPath::new(&path), &data).is_err() {
        log!(
            "Warning: sqlite3: could not write database back to {:?}",
            path
        );
    }
}

fn free_row_allocs(env: &mut Environment, stmt: MutVoidPtr) {
    let allocs = match State::get_mut(env).stmts.get_mut(&stmt) {
        Some(s) => std::mem::take(&mut s.row_allocs),
        None => return,
    };
    for alloc in allocs {
        env.mem.free(alloc);
    }
}

fn sqlite3_open(env: &mut Environment, filename: ConstPtr<u8>, pp_db: MutPtr<MutVoidPtr>) -> i32 {
    let path = env.mem.cstr_at_utf8(filename).unwrap().to_string();
    let existing = if env.fs.is_file(GuestPath::new(&path)) {
        Some(env.fs.read(GuestPath::new(&path)).unwrap())
    } else {
        None
    };
    log_dbg!(
        "sqlite3_open({:?}): {}",
        path,
        match &existing {
            Some(data) => format!("existing file, {} bytes", data.len()),
            None => "new database".to_string(),
        }
    );

    let mut host: *mut ffi::sqlite3 = std::ptr::null_mut();
    let res = unsafe { ffi::sqlite3_open(c":memory:".as_ptr(), &mut host) };
    if res != SQLITE_OK {
        log!("Warning: sqlite3_open({:?}) failed on host: {}", path, res);
        unsafe { ffi::sqlite3_close(host) };
        env.mem.write(pp_db, Ptr::null());
        return res;
    }
    if let Some(data) = existing.filter(|data| !data.is_empty()) {
        let res = unsafe {
            let size = data.len();
            let buf = ffi::sqlite3_malloc64(size as u64) as *mut u8;
            assert!(!buf.is_null());
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf, size);
            ffi::sqlite3_deserialize(
                host,
                c"main".as_ptr(),
                buf,
                size as i64,
                size as i64,
                (ffi::SQLITE_DESERIALIZE_FREEONCLOSE | ffi::SQLITE_DESERIALIZE_RESIZEABLE) as _,
            )
        };
        if res != SQLITE_OK {
            log!(
                "Warning: sqlite3_open({:?}): could not load file content: {}",
                path,
                res
            );
            unsafe { ffi::sqlite3_close(host) };
            env.mem.write(pp_db, Ptr::null());
            return res;
        }
    }

    let handle = env.mem.alloc(4);
    State::get_mut(env).dbs.insert(
        handle,
        DbHostObject {
            host,
            path,
            errmsg: None,
        },
    );
    env.mem.write(pp_db, handle);
    SQLITE_OK
}

fn sqlite3_close(env: &mut Environment, db: MutVoidPtr) -> i32 {
    if db.is_null() {
        return SQLITE_OK;
    }
    let open_stmts: Vec<MutVoidPtr> = State::get_mut(env)
        .stmts
        .iter()
        .filter(|(_, s)| s.db == db)
        .map(|(&k, _)| k)
        .collect();
    // Real SQLite would return SQLITE_BUSY here, but leaking the host object
    // is not useful to us, so just clean up.
    for stmt in open_stmts {
        sqlite3_finalize(env, stmt);
    }
    let Some(db_obj) = State::get_mut(env).dbs.remove(&db) else {
        return SQLITE_MISUSE;
    };
    unsafe { ffi::sqlite3_close(db_obj.host) };
    if let Some(errmsg) = db_obj.errmsg {
        env.mem.free(errmsg.cast());
    }
    env.mem.free(db);
    SQLITE_OK
}

fn sqlite3_errmsg(env: &mut Environment, db: MutVoidPtr) -> ConstPtr<u8> {
    let msg: Vec<u8> = match host_db(env, db) {
        Some(host) => unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(host)) }
            .to_bytes()
            .to_vec(),
        None => b"invalid database handle".to_vec(),
    };
    let new = env.mem.alloc_and_write_cstr(&msg);
    if let Some(db_obj) = State::get_mut(env).dbs.get_mut(&db) {
        if let Some(old) = db_obj.errmsg.replace(new) {
            env.mem.free(old.cast());
        }
    }
    new.cast_const()
}

fn sqlite3_prepare_v2(
    env: &mut Environment,
    db: MutVoidPtr,
    sql: ConstPtr<u8>,
    n_byte: i32,
    pp_stmt: MutPtr<MutVoidPtr>,
    pz_tail: MutPtr<ConstPtr<u8>>,
) -> i32 {
    let Some(host) = host_db(env, db) else {
        return SQLITE_MISUSE;
    };
    let sql_bytes: Vec<u8> = {
        let whole = env.mem.cstr_at(sql);
        if n_byte >= 0 && (n_byte as usize) < whole.len() {
            whole[..n_byte as usize].to_vec()
        } else {
            whole.to_vec()
        }
    };
    log_dbg!(
        "sqlite3_prepare_v2({:?}, {:?})",
        db,
        String::from_utf8_lossy(&sql_bytes)
    );

    let mut host_stmt: *mut ffi::sqlite3_stmt = std::ptr::null_mut();
    let mut tail: *const c_char = std::ptr::null();
    let res = unsafe {
        ffi::sqlite3_prepare_v2(
            host,
            sql_bytes.as_ptr() as *const c_char,
            sql_bytes.len() as c_int,
            &mut host_stmt,
            &mut tail,
        )
    };
    if !pz_tail.is_null() {
        let offset = if tail.is_null() {
            sql_bytes.len()
        } else {
            (tail as usize) - (sql_bytes.as_ptr() as usize)
        };
        env.mem.write(pz_tail, sql + (offset as GuestUSize));
    }
    if res != SQLITE_OK {
        log!(
            "Warning: sqlite3_prepare_v2({:?}) failed: {}",
            String::from_utf8_lossy(&sql_bytes),
            unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(host)) }.to_string_lossy()
        );
    }
    if host_stmt.is_null() {
        env.mem.write(pp_stmt, Ptr::null());
        return res;
    }

    let handle = env.mem.alloc(4);
    State::get_mut(env).stmts.insert(
        handle,
        StmtHostObject {
            host: host_stmt,
            db,
            row_allocs: Vec::new(),
            name_allocs: HashMap::new(),
        },
    );
    env.mem.write(pp_stmt, handle);
    res
}

fn sqlite3_step(env: &mut Environment, stmt: MutVoidPtr) -> i32 {
    free_row_allocs(env, stmt);
    let Some(stmt_obj) = State::get_mut(env).stmts.get(&stmt) else {
        return SQLITE_MISUSE;
    };
    let host = stmt_obj.host;
    let db = stmt_obj.db;
    let res = unsafe { ffi::sqlite3_step(host) };
    if res != SQLITE_ROW && res != SQLITE_DONE {
        log!("Warning: sqlite3_step() returned {}", res);
    }
    if res == SQLITE_DONE {
        let (read_only, autocommit) = unsafe {
            (
                ffi::sqlite3_stmt_readonly(host) != 0,
                ffi::sqlite3_get_autocommit(ffi::sqlite3_db_handle(host)) != 0,
            )
        };
        // Don't write back the database in the middle of a transaction.
        if !read_only && autocommit {
            flush_db(env, db);
        }
    }
    res
}

fn sqlite3_reset(env: &mut Environment, stmt: MutVoidPtr) -> i32 {
    free_row_allocs(env, stmt);
    match host_stmt(env, stmt) {
        Some(host) => unsafe { ffi::sqlite3_reset(host) },
        None => SQLITE_MISUSE,
    }
}

fn sqlite3_finalize(env: &mut Environment, stmt: MutVoidPtr) -> i32 {
    if stmt.is_null() {
        return SQLITE_OK;
    }
    free_row_allocs(env, stmt);
    let Some(stmt_obj) = State::get_mut(env).stmts.remove(&stmt) else {
        return SQLITE_MISUSE;
    };
    for (_, name) in stmt_obj.name_allocs {
        env.mem.free(name.cast());
    }
    env.mem.free(stmt);
    unsafe { ffi::sqlite3_finalize(stmt_obj.host) }
}

fn sqlite3_column_count(env: &mut Environment, stmt: MutVoidPtr) -> i32 {
    match host_stmt(env, stmt) {
        Some(host) => unsafe { ffi::sqlite3_column_count(host) },
        None => 0,
    }
}

fn sqlite3_column_name(env: &mut Environment, stmt: MutVoidPtr, col: i32) -> ConstPtr<u8> {
    let Some(host) = host_stmt(env, stmt) else {
        return Ptr::null();
    };
    if let Some(&name) = State::get_mut(env).stmts[&stmt].name_allocs.get(&col) {
        return name.cast_const();
    }
    let name_ptr = unsafe { ffi::sqlite3_column_name(host, col) };
    if name_ptr.is_null() {
        return Ptr::null();
    }
    let name = unsafe { CStr::from_ptr(name_ptr) }.to_bytes().to_vec();
    let guest_name = env.mem.alloc_and_write_cstr(&name);
    State::get_mut(env)
        .stmts
        .get_mut(&stmt)
        .unwrap()
        .name_allocs
        .insert(col, guest_name);
    guest_name.cast_const()
}

fn sqlite3_column_int(env: &mut Environment, stmt: MutVoidPtr, col: i32) -> i32 {
    match host_stmt(env, stmt) {
        Some(host) => unsafe { ffi::sqlite3_column_int(host, col) },
        None => 0,
    }
}

fn sqlite3_column_int64(env: &mut Environment, stmt: MutVoidPtr, col: i32) -> i64 {
    match host_stmt(env, stmt) {
        Some(host) => unsafe { ffi::sqlite3_column_int64(host, col) },
        None => 0,
    }
}

fn sqlite3_column_double(env: &mut Environment, stmt: MutVoidPtr, col: i32) -> f64 {
    match host_stmt(env, stmt) {
        Some(host) => unsafe { ffi::sqlite3_column_double(host, col) },
        None => 0.0,
    }
}

fn sqlite3_column_bytes(env: &mut Environment, stmt: MutVoidPtr, col: i32) -> i32 {
    match host_stmt(env, stmt) {
        Some(host) => unsafe { ffi::sqlite3_column_bytes(host, col) },
        None => 0,
    }
}

/// Copy a column value into guest memory. The copy is always NUL-terminated,
/// and lives until the next step/reset/finalize of the statement.
fn copy_column_to_guest(env: &mut Environment, stmt: MutVoidPtr, data: Vec<u8>) -> MutVoidPtr {
    let guest = env.mem.alloc_and_write_cstr(&data).cast_void();
    State::get_mut(env)
        .stmts
        .get_mut(&stmt)
        .unwrap()
        .row_allocs
        .push(guest);
    guest
}

fn sqlite3_column_text(env: &mut Environment, stmt: MutVoidPtr, col: i32) -> ConstPtr<u8> {
    let Some(host) = host_stmt(env, stmt) else {
        return Ptr::null();
    };
    let data = unsafe {
        let text = ffi::sqlite3_column_text(host, col);
        if text.is_null() {
            return Ptr::null();
        }
        let len = ffi::sqlite3_column_bytes(host, col);
        std::slice::from_raw_parts(text, len as usize).to_vec()
    };
    copy_column_to_guest(env, stmt, data).cast().cast_const()
}

fn sqlite3_column_blob(env: &mut Environment, stmt: MutVoidPtr, col: i32) -> ConstVoidPtr {
    let Some(host) = host_stmt(env, stmt) else {
        return Ptr::null();
    };
    let data = unsafe {
        let blob = ffi::sqlite3_column_blob(host, col);
        if blob.is_null() {
            return Ptr::null();
        }
        let len = ffi::sqlite3_column_bytes(host, col);
        std::slice::from_raw_parts(blob as *const u8, len as usize).to_vec()
    };
    copy_column_to_guest(env, stmt, data).cast_const()
}

fn sqlite3_bind_parameter_index(
    env: &mut Environment,
    stmt: MutVoidPtr,
    name: ConstPtr<u8>,
) -> i32 {
    let Some(host) = host_stmt(env, stmt) else {
        return 0;
    };
    let mut name = env.mem.cstr_at(name).to_vec();
    name.push(0);
    unsafe { ffi::sqlite3_bind_parameter_index(host, name.as_ptr() as *const c_char) }
}

/// Shared handling of the destructor argument of the `sqlite3_bind_*`
/// functions. We always make SQLite copy the data, so a real destructor would
/// only need to be called to free the guest's own buffer.
fn check_destructor(destructor: u32) {
    // 0 is SQLITE_STATIC, -1 is SQLITE_TRANSIENT.
    if destructor != 0 && destructor != u32::MAX {
        log!(
            "TODO: sqlite3_bind_*: ignoring destructor {:#x}, guest buffer will leak",
            destructor
        );
    }
}

fn sqlite3_bind_text(
    env: &mut Environment,
    stmt: MutVoidPtr,
    idx: i32,
    text: ConstPtr<u8>,
    n: i32,
    destructor: u32,
) -> i32 {
    let Some(host) = host_stmt(env, stmt) else {
        return SQLITE_MISUSE;
    };
    check_destructor(destructor);
    if text.is_null() {
        return unsafe { ffi::sqlite3_bind_null(host, idx) };
    }
    let data: Vec<u8> = if n < 0 {
        env.mem.cstr_at(text).to_vec()
    } else {
        env.mem.bytes_at(text, n as GuestUSize).to_vec()
    };
    unsafe {
        ffi::sqlite3_bind_text(
            host,
            idx,
            data.as_ptr() as *const c_char,
            data.len() as c_int,
            ffi::SQLITE_TRANSIENT(),
        )
    }
}

fn sqlite3_bind_blob(
    env: &mut Environment,
    stmt: MutVoidPtr,
    idx: i32,
    blob: ConstVoidPtr,
    n: i32,
    destructor: u32,
) -> i32 {
    let Some(host) = host_stmt(env, stmt) else {
        return SQLITE_MISUSE;
    };
    check_destructor(destructor);
    if blob.is_null() || n < 0 {
        if n < 0 {
            return SQLITE_ERROR;
        }
        return unsafe { ffi::sqlite3_bind_null(host, idx) };
    }
    let data: Vec<u8> = env.mem.bytes_at(blob.cast::<u8>(), n as GuestUSize).to_vec();
    unsafe {
        ffi::sqlite3_bind_blob(
            host,
            idx,
            data.as_ptr() as *const c_void,
            data.len() as c_int,
            ffi::SQLITE_TRANSIENT(),
        )
    }
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(sqlite3_open(_, _)),
    export_c_func!(sqlite3_close(_)),
    export_c_func!(sqlite3_errmsg(_)),
    export_c_func!(sqlite3_prepare_v2(_, _, _, _, _)),
    export_c_func!(sqlite3_step(_)),
    export_c_func!(sqlite3_reset(_)),
    export_c_func!(sqlite3_finalize(_)),
    export_c_func!(sqlite3_column_count(_)),
    export_c_func!(sqlite3_column_name(_, _)),
    export_c_func!(sqlite3_column_int(_, _)),
    export_c_func!(sqlite3_column_int64(_, _)),
    export_c_func!(sqlite3_column_double(_, _)),
    export_c_func!(sqlite3_column_bytes(_, _)),
    export_c_func!(sqlite3_column_text(_, _)),
    export_c_func!(sqlite3_column_blob(_, _)),
    export_c_func!(sqlite3_bind_parameter_index(_, _)),
    export_c_func!(sqlite3_bind_text(_, _, _, _, _)),
    export_c_func!(sqlite3_bind_blob(_, _, _, _, _)),
];

pub const DYLIB: HostDylib = HostDylib {
    path: "/usr/lib/libsqlite3.dylib",
    aliases: &["/usr/lib/libsqlite3.0.dylib"],
    class_exports: &[],
    constant_exports: &[],
    function_exports: &[FUNCTIONS],
};
