use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::cache;
use crate::manifest::{DependencySpec, Manifest, parse_manifest};
use crate::registry::resolve_registry_dep;

#[derive(Debug, Clone)]
pub struct ResolvedDep {
    pub name: String,
    pub spec: DependencySpec,
    pub source_path: PathBuf,
    pub entry_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DependencyGraph {
    pub root: Manifest,
    pub packages: HashMap<String, ResolvedDep>,
}

/// 从项目根目录解析完整依赖图。
pub fn resolve_dependencies(project_root: &Path) -> Result<DependencyGraph, String> {
    let manifest_path = project_root.join("bolide.toml");
    let manifest = parse_manifest(&manifest_path)?;
    let mut graph = DependencyGraph {
        root: manifest,
        packages: HashMap::new(),
    };

    let mut chain: Vec<String> = Vec::new();
    resolve_node(
        project_root,
        &graph.root,
        &mut graph.packages,
        &mut chain,
    )?;

    Ok(graph)
}

fn resolve_node(
    package_root: &Path,
    manifest: &Manifest,
    packages: &mut HashMap<String, ResolvedDep>,
    chain: &mut Vec<String>,
) -> Result<(), String> {
    if chain.contains(&manifest.package.name) {
        return Err(format!(
            "Circular dependency detected: {} -> {}",
            chain.join(" -> "),
            manifest.package.name
        ));
    }
    chain.push(manifest.package.name.clone());

    for (name, spec) in &manifest.dependencies {
        if packages.contains_key(name) {
            continue;
        }

        let resolved = resolve_single(package_root, name, spec)?;
        let dep_manifest_path = resolved.source_path.join("bolide.toml");
        let dep_manifest = parse_manifest(&dep_manifest_path)
            .map_err(|e| format!("Dependency '{}' manifest error: {}", name, e))?;

        let dep_source_path = resolved.source_path.clone();
        packages.insert(name.clone(), resolved);
        resolve_node(
            &dep_source_path,
            &dep_manifest,
            packages,
            chain,
        )?;
    }

    chain.pop();
    Ok(())
}

fn resolve_single(
    package_root: &Path,
    name: &str,
    spec: &DependencySpec,
) -> Result<ResolvedDep, String> {
    match spec {
        DependencySpec::Git { git, ref_ } => resolve_git(name, git, ref_),
        DependencySpec::GitShort(git) => {
            if let Some((url, ref_)) = git.rsplit_once('#') {
                resolve_git(name, url, ref_)
            } else {
                resolve_git(name, git, "main")
            }
        }
        DependencySpec::Path { path } => resolve_path(package_root, name, path),
        DependencySpec::Registry { version, registry } => {
            let registry = registry.as_deref().unwrap_or("https://registry.bolide.dev");
            resolve_registry_dep(name, version, registry)
        }
    }
}

fn resolve_git(name: &str, url: &str, ref_: &str) -> Result<ResolvedDep, String> {
    let (host, owner, repo) = parse_git_url(url)?;
    let dest = cache::git_cache_path(&host, &owner, &repo, &normalize_ref(ref_));

    // 浅克隆 + 分支对 tag 也适用；git clone --branch 可接受 tag/branch。
    if !dest.exists() {
        cache::ensure_dir(dest.parent().unwrap())?;
        git_clone(url, &dest, ref_)?;
    } else {
        // 简单校验：若 ref 是 tag 或 commit，检查 HEAD 是否匹配
        if is_deterministic_ref(ref_) {
            let current = git_rev_parse(&dest, "HEAD")?;
            let target = if looks_like_commit(ref_) {
                ref_.to_string()
            } else {
                git_rev_parse(&dest, ref_)?
            };
            if current != target {
                // 删除并重新克隆
                let _ = std::fs::remove_dir_all(&dest);
                cache::ensure_dir(dest.parent().unwrap())?;
                git_clone(url, &dest, ref_)?;
            }
        }
    }

    entry_from_source(name, spec_from_git(url, ref_), &dest)
}

fn resolve_path(package_root: &Path, name: &str, path: &str) -> Result<ResolvedDep, String> {
    let abs = package_root.join(path).canonicalize().map_err(|e| {
        format!(
            "Path dependency '{}' not found at '{}': {}",
            name,
            package_root.join(path).display(),
            e
        )
    })?;
    entry_from_source(name, DependencySpec::Path { path: abs.to_string_lossy().to_string() }, &abs)
}

fn entry_from_source(name: &str, spec: DependencySpec, source_path: &Path) -> Result<ResolvedDep, String> {
    let manifest = parse_manifest(&source_path.join("bolide.toml"))
        .map_err(|e| format!("Dependency '{}' is missing or has invalid bolide.toml: {}", name, e))?;
    let entry = source_path.join(&manifest.package.lib);
    if !entry.exists() {
        return Err(format!(
            "Dependency '{}' entry file not found: {}",
            name,
            entry.display()
        ));
    }
    Ok(ResolvedDep {
        name: name.to_string(),
        spec,
        source_path: source_path.to_path_buf(),
        entry_file: entry,
    })
}

fn parse_git_url(url: &str) -> Result<(String, String, String), String> {
    let trimmed = url.trim_end_matches(".git");

    // SSH 形式：git@host:owner/repo
    if let Some(rest) = trimmed.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if segs.len() >= 2 {
                return Ok((
                    host.to_string(),
                    segs[segs.len() - 2].to_string(),
                    segs[segs.len() - 1].to_string(),
                ));
            }
        }
        return Err(format!("Invalid SSH git URL: {}", url));
    }

    // HTTP(S) 形式：https://host/owner/repo
    let no_scheme = trimmed
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let segs: Vec<&str> = no_scheme.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 {
        return Err(format!("Invalid git URL: {}", url));
    }
    Ok((
        segs[0].to_string(),
        segs[segs.len() - 2].to_string(),
        segs[segs.len() - 1].to_string(),
    ))
}

fn normalize_ref(ref_: &str) -> String {
    // 简单规范化：将 / 替换为 -，避免路径问题
    ref_.replace('/', "-")
}

fn looks_like_commit(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_deterministic_ref(ref_: &str) -> bool {
    looks_like_commit(ref_) || ref_.starts_with('v') || ref_.parse::<u64>().is_ok()
}

fn git_clone(url: &str, dest: &Path, ref_: &str) -> Result<(), String> {
    check_git()?;
    let dest_str = dest.to_string_lossy().to_string();

    if looks_like_commit(ref_) {
        // 裸 commit SHA：完整克隆后 checkout（git clone --branch 不接受 SHA）
        run_git(&["clone", url, &dest_str], None)?;
        run_git(&["checkout", ref_], Some(dest))?;
    } else {
        // tag 或分支：浅克隆指定 ref
        run_git(&["clone", "--depth", "1", "--branch", ref_, url, &dest_str], None)?;
    }
    Ok(())
}

fn run_git(args: &[&str], cwd: Option<&Path>) -> Result<(), String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to run `git {}`: {}", args.join(" "), e))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn git_rev_parse(repo: &Path, rev: &str) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("Failed to run git rev-parse in '{}': {}", repo.display(), e))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn check_git() -> Result<(), String> {
    match std::process::Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        _ => Err("Git is required but not found in PATH".to_string()),
    }
}

fn spec_from_git(url: &str, ref_: &str) -> DependencySpec {
    DependencySpec::Git {
        git: url.to_string(),
        ref_: ref_.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_url() {
        assert_eq!(
            parse_git_url("https://github.com/bolide-lang/http.git").unwrap(),
            ("github.com".to_string(), "bolide-lang".to_string(), "http".to_string())
        );
    }

    #[test]
    fn test_parse_git_url_ssh() {
        assert_eq!(
            parse_git_url("git@github.com:bolide-lang/http.git").unwrap(),
            ("github.com".to_string(), "bolide-lang".to_string(), "http".to_string())
        );
    }
}
