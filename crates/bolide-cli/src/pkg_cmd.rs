//! 包管理器 CLI 命令实现：new / add / install / publish。

use std::path::{Path, PathBuf};

use bolide_pkg::{parse_manifest, resolve_dependencies, Lockfile};

const DEFAULT_REGISTRY: &str = "https://registry.bolide.dev";

/// `bolide new <name>`：创建项目骨架。
pub fn new_project(name: &str) -> miette::Result<()> {
    let root = PathBuf::from(name);
    if root.exists() {
        return Err(miette::miette!("Directory '{}' already exists", name));
    }

    let src = root.join("src");
    std::fs::create_dir_all(&src)
        .map_err(|e| miette::miette!("Failed to create '{}': {}", src.display(), e))?;

    let manifest = format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\n\n[dependencies]\n",
        name
    );
    std::fs::write(root.join("bolide.toml"), manifest)
        .map_err(|e| miette::miette!("Failed to write bolide.toml: {}", e))?;

    let main_src = "fn main() -> int {\n    print(\"Hello, world!\");\n    return 0;\n}\n";
    std::fs::write(src.join("main.bl"), main_src)
        .map_err(|e| miette::miette!("Failed to write src/main.bl: {}", e))?;

    println!("Created new Bolide project: {}", name);
    println!("  {}/bolide.toml", name);
    println!("  {}/src/main.bl", name);
    Ok(())
}

/// `bolide add <spec>`：解析依赖并写入 bolide.toml，然后运行 install。
pub fn add_dependency(
    spec: &str,
    tag: Option<&str>,
    is_path: bool,
    registry: Option<&str>,
    name_override: Option<&str>,
) -> miette::Result<()> {
    let project_root = find_project_root_cwd()?;
    let manifest_path = project_root.join("bolide.toml");

    let (dep_name, dep_line) = build_dependency_entry(spec, tag, is_path, registry, name_override)?;

    append_dependency(&manifest_path, &dep_name, &dep_line)?;
    println!("Added dependency '{}' to bolide.toml", dep_name);

    // 解析并写锁文件
    install_in(&project_root)?;
    Ok(())
}

/// `bolide install`：解析 bolide.toml 全部依赖并写 bolide.lock。
pub fn install() -> miette::Result<()> {
    let project_root = find_project_root_cwd()?;
    install_in(&project_root)
}

/// `bolide publish`：第一阶段仅做本地校验。
pub fn publish() -> miette::Result<()> {
    let project_root = find_project_root_cwd()?;
    let manifest =
        parse_manifest(&project_root.join("bolide.toml")).map_err(|e| miette::miette!("{}", e))?;

    let entry = project_root.join(&manifest.package.lib);
    if !entry.exists() {
        return Err(miette::miette!(
            "Entry file '{}' not found",
            entry.display()
        ));
    }

    // 解析依赖以确保所有 import 可达
    resolve_dependencies(&project_root).map_err(|e| miette::miette!("{}", e))?;

    println!(
        "Package '{}' v{} is valid:",
        manifest.package.name, manifest.package.version
    );
    println!("  entry: {}", manifest.package.lib);
    println!("  dependencies: {}", manifest.dependencies.len());
    println!();
    println!("Publishing to a registry is not yet implemented.");
    Ok(())
}

// ---- 内部辅助 ----

fn install_in(project_root: &Path) -> miette::Result<()> {
    let graph = resolve_dependencies(project_root).map_err(|e| miette::miette!("{}", e))?;
    let lockfile = Lockfile::from_graph(&graph);
    let lock_path = project_root.join("bolide.lock");
    lockfile
        .to_file(&lock_path)
        .map_err(|e| miette::miette!("{}", e))?;

    println!("Resolved {} dependencies:", graph.packages.len());
    let mut names: Vec<&String> = graph.packages.keys().collect();
    names.sort();
    for name in names {
        let dep = &graph.packages[name];
        println!("  {} ({})", name, dep.spec.kind());
    }
    println!("Wrote {}", lock_path.display());
    Ok(())
}

fn find_project_root_cwd() -> miette::Result<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        if current.join("bolide.toml").exists() {
            return Ok(current);
        }
        dir = current.parent().map(|p| p.to_path_buf());
    }
    Err(miette::miette!(
        "No bolide.toml found in the current directory or any parent. Run `bolide new` first."
    ))
}

/// 根据 add 参数推断依赖名和 TOML 行。
fn build_dependency_entry(
    spec: &str,
    tag: Option<&str>,
    is_path: bool,
    registry: Option<&str>,
    name_override: Option<&str>,
) -> miette::Result<(String, String)> {
    // 1. 显式路径依赖
    if is_path || is_local_path(spec) {
        let name = name_override
            .map(|s| s.to_string())
            .unwrap_or_else(|| infer_name_from_path(spec));
        let line = format!("{} = {{ path = \"{}\" }}", name, spec);
        return Ok((name, line));
    }

    // 2. git URL
    if is_git_url(spec) {
        let name = name_override
            .map(|s| s.to_string())
            .unwrap_or_else(|| infer_name_from_git(spec));
        let ref_ = tag.unwrap_or("main");
        let line = format!("{} = {{ git = \"{}\", ref = \"{}\" }}", name, spec, ref_);
        return Ok((name, line));
    }

    // 3. registry: name 或 name@version
    let (name, version) = match spec.split_once('@') {
        Some((n, v)) => (n.to_string(), v.to_string()),
        None => {
            return Err(miette::miette!(
                "Registry dependency requires a version: use '{}@<version>'",
                spec
            ))
        }
    };
    let name = name_override.map(|s| s.to_string()).unwrap_or(name);
    let reg = registry.unwrap_or(DEFAULT_REGISTRY);
    let line = format!(
        "{} = {{ version = \"{}\", registry = \"{}\" }}",
        name, version, reg
    );
    Ok((name, line))
}

fn append_dependency(manifest_path: &Path, dep_name: &str, dep_line: &str) -> miette::Result<()> {
    let content = std::fs::read_to_string(manifest_path)
        .map_err(|e| miette::miette!("Failed to read bolide.toml: {}", e))?;

    if content.contains(&format!("\n{} =", dep_name))
        || content.contains(&format!("\n{} =", dep_name.trim()))
    {
        return Err(miette::miette!(
            "Dependency '{}' already exists in bolide.toml",
            dep_name
        ));
    }

    let mut new_content = content.clone();
    if content.contains("[dependencies]") {
        // 在 [dependencies] 段后追加
        new_content = content.replacen(
            "[dependencies]",
            &format!("[dependencies]\n{}", dep_line),
            1,
        );
    } else {
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(&format!("\n[dependencies]\n{}\n", dep_line));
    }

    std::fs::write(manifest_path, new_content)
        .map_err(|e| miette::miette!("Failed to write bolide.toml: {}", e))
}

fn is_git_url(spec: &str) -> bool {
    spec.starts_with("https://")
        || spec.starts_with("http://")
        || spec.starts_with("git@")
        || spec.ends_with(".git")
}

fn is_local_path(spec: &str) -> bool {
    spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with('/')
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
        || (spec.len() >= 2 && spec.as_bytes()[1] == b':') // Windows drive letter
}

fn infer_name_from_git(url: &str) -> String {
    url.trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or("dep")
        .to_string()
}

fn infer_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("dep")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_git_url() {
        assert!(is_git_url("https://github.com/x/y.git"));
        assert!(is_git_url("git@github.com:x/y.git"));
        assert!(!is_git_url("../local"));
    }

    #[test]
    fn test_is_local_path() {
        assert!(is_local_path("../utils"));
        assert!(is_local_path("./utils"));
        assert!(is_local_path("C:\\dev\\utils"));
        assert!(!is_local_path("http"));
    }

    #[test]
    fn test_infer_name_from_git() {
        assert_eq!(
            infer_name_from_git("https://github.com/bolide-lang/http.git"),
            "http"
        );
    }

    #[test]
    fn test_build_path_entry() {
        let (name, line) = build_dependency_entry("../utils", None, true, None, None).unwrap();
        assert_eq!(name, "utils");
        assert!(line.contains("path = \"../utils\""));
    }

    #[test]
    fn test_build_registry_entry() {
        let (name, line) = build_dependency_entry("http@1.2.0", None, false, None, None).unwrap();
        assert_eq!(name, "http");
        assert!(line.contains("version = \"1.2.0\""));
    }
}
