//! Embedded file database for the Bolide standard library.

use crate::list::ElementType;
use crate::{BolideBytes, BolideDict, BolideDynamic, BolideList, BolideString, DynamicType};
use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

#[repr(C)]
pub struct BolideDb {
    root: PathBuf,
    last_error: Mutex<String>,
    tables: RwLock<HashMap<String, Table>>,
}

#[derive(Clone, Debug, PartialEq)]
enum DbValue {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug)]
struct Row {
    id: i64,
    values: Vec<(String, DbValue)>,
}

#[derive(Clone, Debug)]
struct Table {
    columns: Vec<String>,
    next_id: i64,
    rows: Vec<Row>,
}

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn empty_dynamic_dict() -> *mut BolideDict {
    BolideDict::new(ElementType::String, ElementType::Dynamic)
}

fn empty_dict_list() -> *mut BolideList {
    BolideList::new(ElementType::Dict)
}

fn valid_table_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn table_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_table_name(name) {
        return Err(format!("invalid table name '{}'", name));
    }
    Ok(root.join(format!("{}.btable", name)))
}

fn parse_columns_csv(columns: &str) -> Vec<String> {
    columns
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let hi = (bytes[index] as char).to_digit(16)? as u8;
        let lo = (bytes[index + 1] as char).to_digit(16)? as u8;
        out.push((hi << 4) | lo);
        index += 2;
    }
    Some(out)
}

fn encode_field(value: &str) -> String {
    hex_encode(value.as_bytes())
}

fn decode_field(value: &str) -> Option<String> {
    String::from_utf8(hex_decode(value)?).ok()
}

fn encode_value(value: &DbValue) -> String {
    match value {
        DbValue::None => "n".to_string(),
        DbValue::Bool(v) => {
            if *v {
                "b1".to_string()
            } else {
                "b0".to_string()
            }
        }
        DbValue::Int(v) => format!("i{}", v),
        DbValue::Float(v) => format!("f{}", v.to_bits()),
        DbValue::String(v) => format!("s{}", hex_encode(v.as_bytes())),
        DbValue::Bytes(v) => format!("x{}", hex_encode(v)),
    }
}

fn decode_value(value: &str) -> Option<DbValue> {
    let (tag, body) = value.split_at(1);
    match tag {
        "n" => Some(DbValue::None),
        "b" => Some(DbValue::Bool(body == "1")),
        "i" => body.parse::<i64>().ok().map(DbValue::Int),
        "f" => body
            .parse::<u64>()
            .ok()
            .map(|bits| DbValue::Float(f64::from_bits(bits))),
        "s" => decode_field(body).map(DbValue::String),
        "x" => hex_decode(body).map(DbValue::Bytes),
        _ => None,
    }
}

fn load_table(path: &Path) -> Result<Table, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut lines = content.lines();
    if lines.next() != Some("BOLIDE_DB_V1") {
        return Err("invalid database table format".to_string());
    }

    let mut next_id = 1;
    let mut columns = Vec::new();
    let mut rows = Vec::new();

    for line in lines {
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.first().copied() {
            Some("next_id") if parts.len() >= 2 => {
                next_id = parts[1].parse::<i64>().unwrap_or(1).max(1);
            }
            Some("columns") => {
                columns = parts[1..]
                    .iter()
                    .filter_map(|part| decode_field(part))
                    .collect();
            }
            Some("row") if parts.len() >= 2 => {
                let id = parts[1].parse::<i64>().unwrap_or(0);
                if id <= 0 {
                    continue;
                }
                let mut values = Vec::new();
                for part in &parts[2..] {
                    let Some((key, value)) = part.split_once('=') else {
                        continue;
                    };
                    let Some(key) = decode_field(key) else {
                        continue;
                    };
                    let Some(value) = decode_value(value) else {
                        continue;
                    };
                    values.push((key, value));
                }
                rows.push(Row { id, values });
            }
            _ => {}
        }
    }

    Ok(Table {
        columns,
        next_id,
        rows,
    })
}

fn save_table(path: &Path, table: &Table) -> Result<(), String> {
    let mut out = String::new();
    out.push_str("BOLIDE_DB_V1\n");
    out.push_str(&format!("next_id\t{}\n", table.next_id.max(1)));
    out.push_str("columns");
    for column in &table.columns {
        out.push('\t');
        out.push_str(&encode_field(column));
    }
    out.push('\n');

    for row in &table.rows {
        out.push_str("row\t");
        out.push_str(&row.id.to_string());

        let mut values = row.values.clone();
        values.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, value) in values {
            out.push('\t');
            out.push_str(&encode_field(&key));
            out.push('=');
            out.push_str(&encode_value(&value));
        }
        out.push('\n');
    }

    fs::write(path, out).map_err(|e| e.to_string())
}

fn find_value<'a>(row: &'a Row, key: &str) -> Option<&'a DbValue> {
    row.values
        .iter()
        .find_map(|(name, value)| if name == key { Some(value) } else { None })
}

fn set_row_value(row: &mut Row, key: String, value: DbValue) {
    if key == "id" {
        return;
    }
    if let Some((_, existing)) = row.values.iter_mut().find(|(name, _)| *name == key) {
        *existing = value;
    } else {
        row.values.push((key, value));
    }
}

fn db_value_from_dynamic(value: *const BolideDynamic) -> DbValue {
    if value.is_null() {
        return DbValue::None;
    }
    let value_ref = unsafe { &*value };
    match value_ref.tag {
        DynamicType::None => DbValue::None,
        DynamicType::Bool => DbValue::Bool(unsafe { value_ref.data.bool_val != 0 }),
        DynamicType::Int => DbValue::Int(unsafe { value_ref.data.int_val }),
        DynamicType::Float => DbValue::Float(unsafe { value_ref.data.float_val }),
        DynamicType::String => {
            let ptr = unsafe { value_ref.data.string_ptr };
            if ptr.is_null() {
                DbValue::String(String::new())
            } else {
                DbValue::String(unsafe { (&*ptr).as_str().to_string() })
            }
        }
        DynamicType::Bytes => {
            let ptr = unsafe { value_ref.data.bytes_ptr };
            if ptr.is_null() {
                DbValue::Bytes(Vec::new())
            } else {
                DbValue::Bytes(unsafe { (&*ptr).as_slice().to_vec() })
            }
        }
        DynamicType::BigInt | DynamicType::Decimal => DbValue::String(value_ref.to_string_repr()),
        DynamicType::List | DynamicType::Dict => DbValue::String(value_ref.to_string_repr()),
    }
}

fn db_value_from_raw(elem_type: ElementType, raw: i64) -> DbValue {
    match elem_type {
        ElementType::Int => DbValue::Int(raw),
        ElementType::Float => DbValue::Float(f64::from_bits(raw as u64)),
        ElementType::Bool => DbValue::Bool(raw != 0),
        ElementType::String => {
            let ptr = raw as *const BolideString;
            if ptr.is_null() {
                DbValue::String(String::new())
            } else {
                DbValue::String(unsafe { (&*ptr).as_str().to_string() })
            }
        }
        ElementType::Bytes => {
            let ptr = raw as *const BolideBytes;
            if ptr.is_null() {
                DbValue::Bytes(Vec::new())
            } else {
                DbValue::Bytes(unsafe { (&*ptr).as_slice().to_vec() })
            }
        }
        ElementType::Dynamic => db_value_from_dynamic(raw as *const BolideDynamic),
        ElementType::BigInt | ElementType::Decimal | ElementType::List | ElementType::Dict => {
            DbValue::String("{...}".to_string())
        }
        ElementType::Ptr | ElementType::Closure | ElementType::Object => DbValue::None,
    }
}

fn row_values_from_dict(row: *const BolideDict) -> Vec<(String, DbValue)> {
    if row.is_null() {
        return Vec::new();
    }

    let dict = unsafe { &*row };
    let value_type = dict.value_type();
    let mut values = Vec::new();
    for raw_key in dict.keys() {
        let key = if dict.key_type() == ElementType::String {
            let key_ptr = raw_key as *const BolideString;
            if key_ptr.is_null() {
                continue;
            }
            unsafe { (&*key_ptr).as_str().to_string() }
        } else {
            raw_key.to_string()
        };
        if key == "id" {
            continue;
        }
        if let Some(raw_value) = dict.get(raw_key) {
            values.push((key, db_value_from_raw(value_type, raw_value)));
        }
    }
    values
}

fn dynamic_from_db_value(value: &DbValue) -> *mut BolideDynamic {
    match value {
        DbValue::None => BolideDynamic::none(),
        DbValue::Bool(v) => BolideDynamic::from_bool(*v),
        DbValue::Int(v) => BolideDynamic::from_int(*v),
        DbValue::Float(v) => BolideDynamic::from_float(*v),
        DbValue::String(v) => {
            let s = BolideString::new(v);
            BolideDynamic::from_string(s)
        }
        DbValue::Bytes(v) => {
            let bytes = BolideBytes::from_slice(v);
            BolideDynamic::from_bytes(bytes)
        }
    }
}

fn dict_set_db_value(dict: *mut BolideDict, key: &str, value: &DbValue) {
    let key_ptr = BolideString::new(key);
    let dyn_value = dynamic_from_db_value(value);
    unsafe {
        (&mut *dict).set(key_ptr as i64, dyn_value as i64);
    }
    crate::bolide_string_release(key_ptr);
    crate::bolide_dynamic_release(dyn_value);
}

fn dict_from_row(row: &Row) -> *mut BolideDict {
    let dict = empty_dynamic_dict();
    dict_set_db_value(dict, "id", &DbValue::Int(row.id));
    for (key, value) in &row.values {
        dict_set_db_value(dict, key, value);
    }
    dict
}

fn list_from_rows<'a>(rows: impl IntoIterator<Item = &'a Row>) -> *mut BolideList {
    let list = BolideList::new(ElementType::Dict);
    for row in rows {
        let dict = dict_from_row(row);
        unsafe {
            (&mut *list).push(dict as i64);
        }
        crate::bolide_dict_release(dict);
    }
    list
}

fn set_error(db: *mut BolideDb, error: impl Into<String>) {
    if !db.is_null() {
        let db = unsafe { &*db };
        if let Ok(mut last_error) = db.last_error.lock() {
            *last_error = error.into();
        }
    }
}

fn clear_error(db: *mut BolideDb) {
    set_error(db, "");
}

fn table_path_for_db(db: &BolideDb, name: &str) -> Result<PathBuf, String> {
    table_path(&db.root, name)
}

fn load_table_cached(db: &BolideDb, name: &str) -> Result<Table, String> {
    if let Ok(cache) = db.tables.read() {
        if let Some(table) = cache.get(name) {
            return Ok(table.clone());
        }
    }

    let path = table_path_for_db(db, name)?;
    let table = load_table(&path)?;
    if let Ok(mut cache) = db.tables.write() {
        cache.insert(name.to_string(), table.clone());
    }
    Ok(table)
}

fn with_table_mut<F>(db: *mut BolideDb, table_name: *const c_char, create: bool, f: F) -> i64
where
    F: FnOnce(&mut Table) -> Result<i64, String>,
{
    if db.is_null() {
        return 0;
    }
    let Some(name) = cstr_to_str(table_name) else {
        set_error(db, "null table name");
        return 0;
    };

    let db_ref = unsafe { &*db };
    let path = match table_path_for_db(db_ref, name) {
        Ok(path) => path,
        Err(err) => {
            set_error(db, err);
            return 0;
        }
    };

    let mut cache = match db_ref.tables.write() {
        Ok(cache) => cache,
        Err(_) => {
            set_error(db, "database table cache is poisoned");
            return 0;
        }
    };
    let mut table = if let Some(table) = cache.get(name) {
        table.clone()
    } else if path.exists() {
        match load_table(&path) {
            Ok(table) => table,
            Err(err) => {
                set_error(db, err);
                return 0;
            }
        }
    } else if create {
        Table {
            columns: Vec::new(),
            next_id: 1,
            rows: Vec::new(),
        }
    } else {
        set_error(db, format!("table '{}' does not exist", name));
        return 0;
    };

    let result = match f(&mut table) {
        Ok(result) => result,
        Err(err) => {
            set_error(db, err);
            return 0;
        }
    };

    if let Err(err) = save_table(&path, &table) {
        set_error(db, err);
        return 0;
    }
    cache.insert(name.to_string(), table);
    clear_error(db);
    result
}

#[no_mangle]
pub extern "C" fn bolide_db_open(path: *const c_char) -> *mut BolideDb {
    let Some(path) = cstr_to_str(path) else {
        return std::ptr::null_mut();
    };
    if fs::create_dir_all(path).is_err() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(BolideDb {
        root: PathBuf::from(path),
        last_error: Mutex::new(String::new()),
        tables: RwLock::new(HashMap::new()),
    }))
}

#[no_mangle]
pub extern "C" fn bolide_db_close(db: *mut BolideDb) {
    if db.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(db);
    }
}

#[no_mangle]
pub extern "C" fn bolide_db_last_error(db: *const BolideDb) -> *mut BolideString {
    if db.is_null() {
        return BolideString::new("database is closed");
    }
    let db = unsafe { &*db };
    match db.last_error.lock() {
        Ok(last_error) => BolideString::new(last_error.as_str()),
        Err(_) => BolideString::new("database error state is poisoned"),
    }
}

#[no_mangle]
pub extern "C" fn bolide_db_create_table(
    db: *mut BolideDb,
    table_name: *const c_char,
    columns: *const c_char,
) -> i64 {
    if db.is_null() {
        return 0;
    }
    let Some(name) = cstr_to_str(table_name) else {
        set_error(db, "null table name");
        return 0;
    };
    let columns = cstr_to_str(columns).unwrap_or("");

    let db_ref = unsafe { &*db };
    let path = match table_path_for_db(db_ref, name) {
        Ok(path) => path,
        Err(err) => {
            set_error(db, err);
            return 0;
        }
    };

    let mut cache = match db_ref.tables.write() {
        Ok(cache) => cache,
        Err(_) => {
            set_error(db, "database table cache is poisoned");
            return 0;
        }
    };
    if cache.contains_key(name) {
        clear_error(db);
        return 1;
    }
    if path.exists() {
        if let Ok(table) = load_table(&path) {
            cache.insert(name.to_string(), table);
        }
        clear_error(db);
        return 1;
    }

    let table = Table {
        columns: parse_columns_csv(columns),
        next_id: 1,
        rows: Vec::new(),
    };
    match save_table(&path, &table) {
        Ok(()) => {
            cache.insert(name.to_string(), table);
            clear_error(db);
            1
        }
        Err(err) => {
            set_error(db, err);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_db_insert(
    db: *mut BolideDb,
    table_name: *const c_char,
    row: *const BolideDict,
) -> i64 {
    let values = row_values_from_dict(row);
    with_table_mut(db, table_name, true, |table| {
        for (key, _) in &values {
            if !table.columns.iter().any(|column| column == key) {
                table.columns.push(key.clone());
            }
        }

        let id = table.next_id.max(1);
        table.next_id = id + 1;
        table.rows.push(Row { id, values });
        Ok(id)
    })
}

#[no_mangle]
pub extern "C" fn bolide_db_update(
    db: *mut BolideDb,
    table_name: *const c_char,
    id: i64,
    row: *const BolideDict,
) -> i64 {
    let values = row_values_from_dict(row);
    with_table_mut(db, table_name, false, |table| {
        let Some(existing) = table.rows.iter_mut().find(|row| row.id == id) else {
            return Ok(0);
        };
        for (key, value) in values {
            if !table.columns.iter().any(|column| column == &key) {
                table.columns.push(key.clone());
            }
            set_row_value(existing, key, value);
        }
        Ok(1)
    })
}

#[no_mangle]
pub extern "C" fn bolide_db_delete(db: *mut BolideDb, table_name: *const c_char, id: i64) -> i64 {
    with_table_mut(db, table_name, false, |table| {
        let before = table.rows.len();
        table.rows.retain(|row| row.id != id);
        Ok((before != table.rows.len()) as i64)
    })
}

#[no_mangle]
pub extern "C" fn bolide_db_get(
    db: *mut BolideDb,
    table_name: *const c_char,
    id: i64,
) -> *mut BolideDict {
    if db.is_null() {
        return empty_dynamic_dict();
    }
    let Some(name) = cstr_to_str(table_name) else {
        set_error(db, "null table name");
        return empty_dynamic_dict();
    };
    match load_table_cached(unsafe { &*db }, name) {
        Ok(table) => {
            clear_error(db);
            table
                .rows
                .iter()
                .find(|row| row.id == id)
                .map(dict_from_row)
                .unwrap_or_else(empty_dynamic_dict)
        }
        Err(err) => {
            set_error(db, err);
            empty_dynamic_dict()
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_db_all(db: *mut BolideDb, table_name: *const c_char) -> *mut BolideList {
    if db.is_null() {
        return empty_dict_list();
    }
    let Some(name) = cstr_to_str(table_name) else {
        set_error(db, "null table name");
        return empty_dict_list();
    };
    match load_table_cached(unsafe { &*db }, name) {
        Ok(table) => {
            clear_error(db);
            list_from_rows(&table.rows)
        }
        Err(err) => {
            set_error(db, err);
            empty_dict_list()
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_db_where_eq(
    db: *mut BolideDb,
    table_name: *const c_char,
    column: *const c_char,
    value: *const BolideDynamic,
) -> *mut BolideList {
    if db.is_null() {
        return empty_dict_list();
    }
    let Some(name) = cstr_to_str(table_name) else {
        set_error(db, "null table name");
        return empty_dict_list();
    };
    let Some(column) = cstr_to_str(column) else {
        set_error(db, "null column name");
        return empty_dict_list();
    };
    let expected = db_value_from_dynamic(value);

    match load_table_cached(unsafe { &*db }, name) {
        Ok(table) => {
            clear_error(db);
            let rows = table.rows.iter().filter(|row| {
                if column == "id" {
                    return DbValue::Int(row.id) == expected;
                }
                find_value(row, column).is_some_and(|actual| *actual == expected)
            });
            list_from_rows(rows)
        }
        Err(err) => {
            set_error(db, err);
            empty_dict_list()
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_db_count(db: *mut BolideDb, table_name: *const c_char) -> i64 {
    if db.is_null() {
        return 0;
    }
    let Some(name) = cstr_to_str(table_name) else {
        set_error(db, "null table name");
        return 0;
    };
    match load_table_cached(unsafe { &*db }, name) {
        Ok(table) => {
            clear_error(db);
            table.rows.len() as i64
        }
        Err(err) => {
            set_error(db, err);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path() -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        std::env::temp_dir().join(format!("bolide-db-test-{}", millis))
    }

    #[test]
    fn stores_and_loads_rows() {
        let path = temp_db_path();
        fs::create_dir_all(&path).unwrap();
        let table_path = table_path(&path, "posts").unwrap();
        let table = Table {
            columns: vec!["title".to_string()],
            next_id: 2,
            rows: vec![Row {
                id: 1,
                values: vec![("title".to_string(), DbValue::String("Hello".to_string()))],
            }],
        };
        save_table(&table_path, &table).unwrap();
        let loaded = load_table(&table_path).unwrap();
        assert_eq!(loaded.next_id, 2);
        assert_eq!(loaded.rows[0].id, 1);
        assert_eq!(
            find_value(&loaded.rows[0], "title"),
            Some(&DbValue::String("Hello".to_string()))
        );
        let _ = fs::remove_dir_all(&path);
    }
}
