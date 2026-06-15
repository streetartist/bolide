use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Manifest {
    pub package: PackageMeta,
    #[serde(default)]
    pub dependencies: HashMap<String, DependencySpec>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PackageMeta {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub authors: Option<Vec<String>>,
    pub license: Option<String>,
    #[serde(default = "default_lib_path")]
    pub lib: String,
}

fn default_lib_path() -> String {
    "src/lib.bl".to_string()
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum DependencySpec {
    Git {
        git: String,
        #[serde(default = "default_git_ref", rename = "ref")]
        ref_: String,
    },
    Path {
        path: String,
    },
    Registry {
        version: String,
        #[serde(default)]
        registry: Option<String>,
    },
    GitShort(String),
}

impl DependencySpec {
    pub fn kind(&self) -> &'static str {
        match self {
            DependencySpec::Git { .. } | DependencySpec::GitShort(_) => "git",
            DependencySpec::Path { .. } => "path",
            DependencySpec::Registry { .. } => "registry",
        }
    }
}

fn default_git_ref() -> String {
    "main".to_string()
}

pub fn parse_manifest(path: &Path) -> Result<Manifest, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read manifest '{}': {}", path.display(), e))?;
    toml::from_str(&content)
        .map_err(|e| format!("Failed to parse manifest '{}': {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_manifest() {
        let text = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
http = { git = "https://github.com/bolide-lang/http.git", ref = "v1.2.0" }
utils = { path = "../utils" }
"#;
        let manifest: Manifest = toml::from_str(text).unwrap();
        assert_eq!(manifest.package.name, "demo");
        assert_eq!(manifest.package.lib, "src/lib.bl");
        assert_eq!(manifest.dependencies.len(), 2);
    }

    #[test]
    fn test_parse_registry_dependency() {
        let text = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
db = { version = "0.3.0", registry = "https://registry.bolide.dev" }
"#;
        let manifest: Manifest = toml::from_str(text).unwrap();
        assert!(matches!(
            manifest.dependencies.get("db"),
            Some(DependencySpec::Registry { .. })
        ));
    }
}
