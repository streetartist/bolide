//! 路径依赖端到端解析测试（离线，不触网/git）。
//!
//! 构造一个临时项目，声明一个本地 path 依赖，验证依赖图与入口文件解析。

use std::fs;
use std::path::PathBuf;

use bolide_pkg::{resolve_dependencies, Lockfile};

/// 创建一个唯一的临时目录用于测试隔离。
fn temp_root(tag: &str) -> PathBuf {
    let base = std::env::temp_dir();
    let unique = format!("bolide_pkg_test_{}_{}", tag, std::process::id());
    let dir = base.join(unique);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

#[test]
fn resolves_path_dependency_and_entry() {
    let root = temp_root("path_dep");

    // 依赖包 utils
    let utils = root.join("utils");
    write(
        &utils.join("bolide.toml"),
        "[package]\nname = \"utils\"\nversion = \"0.1.0\"\n",
    );
    write(
        &utils.join("src").join("lib.bl"),
        "fn greet() -> str { return \"hi\"; }\n",
    );

    // 主项目，依赖 utils（相对路径）
    let app = root.join("app");
    write(
        &app.join("bolide.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nutils = { path = \"../utils\" }\n",
    );
    write(
        &app.join("src").join("main.bl"),
        "import utils;\nfn main() -> int { return 0; }\n",
    );

    let graph = resolve_dependencies(&app).expect("resolution should succeed");
    assert_eq!(graph.packages.len(), 1);
    let dep = graph.packages.get("utils").expect("utils resolved");
    assert!(dep.entry_file.ends_with("lib.bl"));
    assert!(dep.entry_file.exists());

    // 锁文件包含 utils
    let lock = Lockfile::from_graph(&graph);
    assert_eq!(lock.packages.len(), 1);
    assert_eq!(lock.packages[0].name, "utils");
    assert!(lock.packages[0].source.starts_with("path+file://"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolves_transitive_path_dependencies() {
    let root = temp_root("transitive");

    // base <- mid <- app
    let base = root.join("base");
    write(
        &base.join("bolide.toml"),
        "[package]\nname = \"base\"\nversion = \"0.1.0\"\n",
    );
    write(&base.join("src").join("lib.bl"), "fn b() -> int { return 1; }\n");

    let mid = root.join("mid");
    write(
        &mid.join("bolide.toml"),
        "[package]\nname = \"mid\"\nversion = \"0.1.0\"\n\n[dependencies]\nbase = { path = \"../base\" }\n",
    );
    write(&mid.join("src").join("lib.bl"), "import base;\nfn m() -> int { return 2; }\n");

    let app = root.join("app");
    write(
        &app.join("bolide.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmid = { path = \"../mid\" }\n",
    );
    write(&app.join("src").join("main.bl"), "import mid;\nfn main() -> int { return 0; }\n");

    let graph = resolve_dependencies(&app).expect("resolution should succeed");
    assert_eq!(graph.packages.len(), 2, "should resolve mid and base");
    assert!(graph.packages.contains_key("mid"));
    assert!(graph.packages.contains_key("base"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn errors_on_missing_path_dependency() {
    let root = temp_root("missing");
    let app = root.join("app");
    write(
        &app.join("bolide.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nghost = { path = \"../ghost\" }\n",
    );

    let result = resolve_dependencies(&app);
    assert!(result.is_err(), "missing dependency should error");

    let _ = fs::remove_dir_all(&root);
}
