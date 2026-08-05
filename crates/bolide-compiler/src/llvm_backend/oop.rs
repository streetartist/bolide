//! Class / ADT / operator metadata for the LLVM backend.
//! Shared pure tables; IR emission stays in `codegen.rs`.

use bolide_parser::{ClassDef, EnumDef, FuncDef, Type};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub name: String,
    pub parent: Option<String>,
    pub fields: Vec<FieldInfo>,
    pub methods: HashMap<String, FuncDef>,
    pub size: usize,
    pub tag: i64,
}

#[derive(Clone, Debug)]
pub struct AdtFieldInfo {
    pub name: Option<String>,
    pub ty: Type,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub struct AdtVariantInfo {
    pub name: String,
    pub tag: i64,
    pub fields: Vec<AdtFieldInfo>,
}

#[derive(Clone, Debug)]
pub struct AdtInfo {
    pub name: String,
    pub variants: Vec<AdtVariantInfo>,
    pub size: usize,
}

pub fn collect_classes(program: &bolide_parser::Program) -> Result<HashMap<String, ClassInfo>, String> {
    let mut raw: HashMap<String, &ClassDef> = HashMap::new();
    for stmt in &program.statements {
        if let bolide_parser::Statement::ClassDef(c) = stmt {
            raw.insert(c.name.clone(), c);
        }
    }
    // parents first
    let mut order = Vec::new();
    let mut visiting = std::collections::HashSet::new();
    let mut done = std::collections::HashSet::new();
    fn visit(
        name: &str,
        raw: &HashMap<String, &ClassDef>,
        order: &mut Vec<String>,
        visiting: &mut std::collections::HashSet<String>,
        done: &mut std::collections::HashSet<String>,
    ) -> Result<(), String> {
        if done.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name.to_string()) {
            return Err(format!("class inheritance cycle involving '{}'", name));
        }
        if let Some(c) = raw.get(name) {
            if let Some(p) = &c.parent {
                if raw.contains_key(p) {
                    visit(p, raw, order, visiting, done)?;
                }
            }
        }
        visiting.remove(name);
        done.insert(name.to_string());
        order.push(name.to_string());
        Ok(())
    }
    for name in raw.keys() {
        visit(name, &raw, &mut order, &mut visiting, &mut done)?;
    }

    let mut classes: HashMap<String, ClassInfo> = HashMap::new();
    let mut tag: i64 = 100;
    for name in order {
        let c = raw.get(&name).unwrap();
        let mut fields = Vec::new();
        let mut offset = 0usize;
        if let Some(p) = &c.parent {
            if let Some(pi) = classes.get(p) {
                fields = pi.fields.clone();
                offset = pi.size;
            }
        }
        for f in &c.fields {
            fields.push(FieldInfo {
                name: f.name.clone(),
                ty: f.ty.clone(),
                offset,
            });
            offset += 8;
        }
        let mut methods = HashMap::new();
        for m in &c.methods {
            methods.insert(m.name.clone(), m.clone());
        }
        // inherit methods not overridden
        if let Some(p) = &c.parent {
            if let Some(pi) = classes.get(p) {
                for (mn, md) in &pi.methods {
                    methods.entry(mn.clone()).or_insert_with(|| md.clone());
                }
            }
        }
        classes.insert(
            name.clone(),
            ClassInfo {
                name: name.clone(),
                parent: c.parent.clone(),
                fields,
                methods,
                size: offset,
                tag,
            },
        );
        tag += 1;
    }
    Ok(classes)
}

pub fn collect_adts(program: &bolide_parser::Program) -> Result<HashMap<String, AdtInfo>, String> {
    let mut adts = HashMap::new();
    for stmt in &program.statements {
        if let bolide_parser::Statement::EnumDef(def) = stmt {
            adts.insert(def.name.clone(), build_adt(def)?);
        }
    }
    Ok(adts)
}

fn build_adt(def: &EnumDef) -> Result<AdtInfo, String> {
    let mut max_fields = 0usize;
    let mut variants = Vec::new();
    for (idx, v) in def.variants.iter().enumerate() {
        max_fields = max_fields.max(v.fields.len());
        let fields = v
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| AdtFieldInfo {
                name: f.name.clone(),
                ty: f.ty.clone(),
                offset: 8 + i * 8,
            })
            .collect();
        variants.push(AdtVariantInfo {
            name: v.name.clone(),
            tag: idx as i64,
            fields,
        });
    }
    Ok(AdtInfo {
        name: def.name.clone(),
        variants,
        size: 8 + max_fields * 8,
    })
}

pub fn type_llvm(ty: &Type) -> &'static str {
    match ty {
        Type::Float => "double",
        Type::Str
        | Type::BigInt
        | Type::Bytes
        | Type::List(_)
        | Type::Dict(_, _)
        | Type::Custom(_)
        | Type::Dyn(_)
        | Type::Adt(_, _)
        | Type::Ptr
        | Type::Channel(_)
        | Type::Func
        | Type::FuncSig(_, _)
        | Type::Future
        | Type::Tuple(_)
        | Type::Weak(_)
        | Type::Unowned(_)
        | Type::Dynamic => "ptr",
        Type::Generic(_) => "i64",
        _ => "i64",
    }
}

pub fn method_full_name(class: &str, method: &str) -> String {
    format!("{}_{}", class, method)
}

pub fn field_offset(class: &ClassInfo, name: &str) -> Option<usize> {
    class.fields.iter().find(|f| f.name == name).map(|f| f.offset)
}

pub fn field_type<'a>(class: &'a ClassInfo, name: &str) -> Option<&'a Type> {
    class.fields.iter().find(|f| f.name == name).map(|f| &f.ty)
}
