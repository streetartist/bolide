use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::manifest::DependencySpec;
use crate::resolve::DependencyGraph;

#[derive(Debug, Serialize, Deserialize)]
pub struct Lockfile {
    pub packages: Vec<LockedPackage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub source: String,
}

impl Lockfile {
    pub fn from_graph(graph: &DependencyGraph) -> Self {
        let mut packages: Vec<LockedPackage> = graph
            .packages
            .values()
            .map(|dep| LockedPackage {
                name: dep.name.clone(),
                source: source_string(&dep.spec,
                    &dep.source_path,
                ),
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Self { packages }
    }

    pub fn to_file(&self, path: &Path) -> Result<(), String> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize lockfile: {}", e))?;
        std::fs::write(path, content)
            .map_err(|e| format!("Failed to write lockfile '{}': {}", path.display(), e))
    }

    pub fn from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read lockfile '{}': {}", path.display(), e))?;
        toml::from_str(&content)
            .map_err(|e| format!("Failed to parse lockfile '{}': {}", path.display(), e))
    }
}

pub fn source_string(spec: &DependencySpec, source_path: &Path) -> String {
    match spec {
        DependencySpec::Git { git, ref_ } => {
            let commit = resolve_git_commit(source_path, ref_);
            format!("git+{}#{}", git, commit.as_deref().unwrap_or(ref_))
        }
        DependencySpec::GitShort(git) => {
            if let Some((url, ref_)) = git.rsplit_once('#') {
                format!("git+{}#{}", url, ref_)
            } else {
                format!("git+{}", git)
            }
        }
        DependencySpec::Path { .. } => format!("path+file://{}", source_path.display()),
        DependencySpec::Registry { version, registry } => {
            let registry = registry.as_deref().unwrap_or("https://registry.bolide.dev");
            format!("registry+{}#{}", registry, version)
        }
    }
}

fn resolve_git_commit(source_path: &Path, _ref_: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(source_path)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, PackageMeta};
    use crate::resolve::ResolvedDep;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_lockfile_roundtrip() {
        let graph = DependencyGraph {
            root: Manifest {
                package: PackageMeta {
                    name: "app".to_string(),
                    version: "0.1.0".to_string(),
                    description: None,
                    authors: None,
                    license: None,
                    lib: "src/lib.bl".to_string(),
                },
                dependencies: HashMap::new(),
            },
            packages: {
                let mut m = HashMap::new();
                m.insert(
                    "http".to_string(),
                    ResolvedDep {
                        name: "http".to_string(),
                        spec: DependencySpec::Path { path: "../http".to_string() },
                        source_path: PathBuf::from("/tmp/http"),
                        entry_file: PathBuf::from("/tmp/http/src/lib.bl"),
                    },
                );
                m
            },
        };
        let lock = Lockfile::from_graph(&graph);
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.packages[0].name, "http");
    }
}
