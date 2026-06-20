//! SQLite database bindings for the Bolide standard library.

use crate::dict::BolideDict;
use crate::list::{BolideList, ElementType};
use crate::string::BolideString;
use crate::BolideDynamic;
use rusqlite::types::Value as RusqliteValue;
use rusqlite::Connection;
use std::cell::RefCell;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr;

#[repr(C)]
pub struct BolideSqlite {
    conn: Connection,
    last_error: RefCell<String>,
}

#[repr(C)]
pub struct BolideSqliteStmt {
    columns: Vec<String>,
    rows: Vec<Vec<RusqliteValue>>,
    index: usize,
}

fn cstr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

fn set_error(db: *mut BolideSqlite, msg: impl Into<String>) {
    if !db.is_null() {
        unsafe {
            *(*db).last_error.borrow_mut() = msg.into();
        }
    }
}

fn clear_error(db: *mut BolideSqlite) {
    set_error(db, "");
}

fn dynamic_from_rusqlite(value: &RusqliteValue) -> *mut BolideDynamic {
    match value {
        RusqliteValue::Null => BolideDynamic::from_int(0),
        RusqliteValue::Integer(i) => BolideDynamic::from_int(*i),
        RusqliteValue::Real(f) => BolideDynamic::from_float(*f),
        RusqliteValue::Text(s) => BolideDynamic::from_string(BolideString::new(s)),
        RusqliteValue::Blob(b) => {
            let hex: String = b.iter().map(|byte| format!("{:02x}", byte)).collect();
            BolideDynamic::from_string(BolideString::new(&hex))
        }
    }
}

fn rusqlite_from_dynamic(d: &BolideDynamic) -> RusqliteValue {
    match d.tag {
        crate::dynamic::DynamicType::Int => RusqliteValue::Integer(d.to_int()),
        crate::dynamic::DynamicType::Float => RusqliteValue::Real(d.to_float()),
        crate::dynamic::DynamicType::Bool => {
            RusqliteValue::Integer(if d.is_truthy() { 1 } else { 0 })
        }
        crate::dynamic::DynamicType::String => RusqliteValue::Text(d.to_string_repr()),
        crate::dynamic::DynamicType::None => RusqliteValue::Null,
        _ => RusqliteValue::Text(d.to_string_repr()),
    }
}

fn extract_params(param_list: *const BolideList) -> Vec<RusqliteValue> {
    if param_list.is_null() {
        return Vec::new();
    }
    let list = unsafe { &*param_list };
    let count = list.len();
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        if let Some(raw) = list.get(i) {
            if raw == 0 {
                values.push(RusqliteValue::Null);
                continue;
            }
            let d = unsafe { &*(raw as *const BolideDynamic) };
            values.push(rusqlite_from_dynamic(d));
        } else {
            values.push(RusqliteValue::Null);
        }
    }
    values
}

fn collect_rows(
    db: *mut BolideSqlite,
    columns: &[String],
    mapped: impl Iterator<Item = Result<Vec<RusqliteValue>, rusqlite::Error>>,
) -> *mut BolideList {
    let result = BolideList::new(ElementType::Dict);
    for row_result in mapped {
        if let Ok(values) = row_result {
            let dict = BolideDict::new(ElementType::String, ElementType::Dynamic);
            for (col, val) in columns.iter().zip(values.iter()) {
                let key_ptr = BolideString::new(col);
                let dyn_val = dynamic_from_rusqlite(val);
                unsafe {
                    (&mut *dict).set(key_ptr as i64, dyn_val as i64);
                }
                crate::bolide_string_release(key_ptr);
                crate::bolide_dynamic_release(dyn_val);
            }
            unsafe {
                (&mut *result).push(dict as i64);
            }
            crate::bolide_dict_release(dict);
        }
    }
    clear_error(db);
    result
}

fn run_query(db: *mut BolideSqlite, sql: &str, param_list: *const BolideList) -> *mut BolideList {
    let empty = || BolideList::new(ElementType::Dict);
    let Some(db_ref) = (unsafe { db.as_ref() }) else {
        return empty();
    };

    let rusqlite_values = extract_params(param_list);
    let rusqlite_refs: Vec<&dyn rusqlite::types::ToSql> = rusqlite_values
        .iter()
        .map(|v| v as &dyn rusqlite::types::ToSql)
        .collect();

    let mut stmt = match db_ref.conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            set_error(db, format!("{}", e));
            return empty();
        }
    };

    let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let rows = stmt.query_map(rusqlite_refs.as_slice(), |row| {
        let mut values = Vec::new();
        for i in 0..columns.len() {
            values.push(
                row.get::<_, RusqliteValue>(i)
                    .unwrap_or(RusqliteValue::Null),
            );
        }
        Ok(values)
    });

    match rows {
        Ok(mapped) => collect_rows(db, &columns, mapped),
        Err(e) => {
            set_error(db, format!("{}", e));
            empty()
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_open(path: *const c_char) -> *mut BolideSqlite {
    let Some(path) = cstr(path) else {
        return ptr::null_mut();
    };
    match Connection::open(path) {
        Ok(conn) => Box::into_raw(Box::new(BolideSqlite {
            conn,
            last_error: RefCell::new(String::new()),
        })),
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_close(db: *mut BolideSqlite) {
    if !db.is_null() {
        unsafe {
            drop(Box::from_raw(db));
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_last_error(db: *const BolideSqlite) -> *mut BolideString {
    if db.is_null() {
        return BolideString::new("");
    }
    let msg = unsafe { (*db).last_error.borrow().clone() };
    BolideString::new(&msg)
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_execute(db: *mut BolideSqlite, sql: *const c_char) -> i64 {
    let Some(db_ref) = (unsafe { db.as_ref() }) else {
        return -1;
    };
    let Some(sql) = cstr(sql) else {
        set_error(db, "execute: null SQL");
        return -1;
    };
    match db_ref.conn.execute(sql, []) {
        Ok(changes) => {
            clear_error(db);
            changes as i64
        }
        Err(e) => {
            set_error(db, format!("{}", e));
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_exec_p(
    db: *mut BolideSqlite,
    sql: *const c_char,
    param_list: *const BolideList,
) -> i64 {
    let Some(db_ref) = (unsafe { db.as_ref() }) else {
        return -1;
    };
    let Some(sql) = cstr(sql) else {
        set_error(db, "exec_p: null SQL");
        return -1;
    };
    let rusqlite_values = extract_params(param_list);
    let rusqlite_refs: Vec<&dyn rusqlite::types::ToSql> = rusqlite_values
        .iter()
        .map(|v| v as &dyn rusqlite::types::ToSql)
        .collect();
    match db_ref.conn.execute(sql, rusqlite_refs.as_slice()) {
        Ok(changes) => {
            clear_error(db);
            changes as i64
        }
        Err(e) => {
            set_error(db, format!("{}", e));
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_query(
    db: *mut BolideSqlite,
    sql: *const c_char,
) -> *mut BolideList {
    let empty = || BolideList::new(ElementType::Dict);
    let Some(sql_str) = cstr(sql) else {
        set_error(db, "query: null SQL");
        return empty();
    };
    run_query(db, sql_str, ptr::null())
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_query_p(
    db: *mut BolideSqlite,
    sql: *const c_char,
    param_list: *const BolideList,
) -> *mut BolideList {
    let empty = || BolideList::new(ElementType::Dict);
    let Some(sql_str) = cstr(sql) else {
        set_error(db, "query_p: null SQL");
        return empty();
    };
    run_query(db, sql_str, param_list)
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_prepare(
    db: *mut BolideSqlite,
    sql: *const c_char,
) -> *mut BolideSqliteStmt {
    let Some(db_ref) = (unsafe { db.as_ref() }) else {
        return ptr::null_mut();
    };
    let Some(sql) = cstr(sql) else {
        set_error(db, "prepare: null SQL");
        return ptr::null_mut();
    };
    let mut stmt = match db_ref.conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            set_error(db, format!("{}", e));
            return ptr::null_mut();
        }
    };
    let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows: Vec<Vec<RusqliteValue>> = match stmt.query_map([], |row| {
        let mut values = Vec::new();
        for i in 0..columns.len() {
            values.push(
                row.get::<_, RusqliteValue>(i)
                    .unwrap_or(RusqliteValue::Null),
            );
        }
        Ok(values)
    }) {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            set_error(db, format!("{}", e));
            return ptr::null_mut();
        }
    };
    clear_error(db);
    Box::into_raw(Box::new(BolideSqliteStmt {
        columns,
        rows,
        index: 0,
    }))
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_step(stmt: *mut BolideSqliteStmt) -> i64 {
    if stmt.is_null() {
        return -1;
    }
    let s = unsafe { &mut *stmt };
    if s.index < s.rows.len() {
        s.index += 1;
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_column_count(stmt: *const BolideSqliteStmt) -> i64 {
    if stmt.is_null() {
        return 0;
    }
    unsafe { (*stmt).columns.len() as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_column_name(
    stmt: *const BolideSqliteStmt,
    index: i64,
) -> *mut BolideString {
    if stmt.is_null() {
        return BolideString::new("");
    }
    let s = unsafe { &*stmt };
    s.columns
        .get(index as usize)
        .map(|name| BolideString::new(name))
        .unwrap_or_else(|| BolideString::new(""))
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_column_value(
    stmt: *const BolideSqliteStmt,
    index: i64,
) -> *mut BolideDynamic {
    let empty = || BolideDynamic::from_int(0);
    if stmt.is_null() {
        return empty();
    }
    let s = unsafe { &*stmt };
    if s.index == 0 || s.index > s.rows.len() {
        return empty();
    }
    let row = &s.rows[s.index - 1];
    row.get(index as usize)
        .map(dynamic_from_rusqlite)
        .unwrap_or_else(empty)
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_finalize(stmt: *mut BolideSqliteStmt) {
    if !stmt.is_null() {
        unsafe {
            drop(Box::from_raw(stmt));
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_sqlite_last_insert_rowid(db: *const BolideSqlite) -> i64 {
    if db.is_null() {
        return 0;
    }
    unsafe { (*db).conn.last_insert_rowid() }
}
