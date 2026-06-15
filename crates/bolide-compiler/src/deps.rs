//! 编译器侧的依赖映射。
//!
//! 这是包管理器与编译器之间的最小接口：仅包含包名到源码目录/入口文件的映射，
//! 不依赖 `bolide-pkg`（避免把 registry/HTTP 等重型依赖带入编译器）。
//! 由 CLI 在解析依赖图后构造并注入。

use std::collections::HashMap;
use std::path::PathBuf;

/// 包名 -> 源码根目录 + 入口文件。
#[derive(Debug, Clone, Default)]
pub struct DependencyManifest {
    /// 包名 -> 包源码根目录（用于 `import "pkg/file.bl";` 形式的相对解析）
    pub packages: HashMap<String, PathBuf>,
    /// 包名 -> 入口文件（用于 `import pkg;` 形式，取自各包 manifest 的 lib 字段）
    pub entries: HashMap<String, PathBuf>,
}

impl DependencyManifest {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个已解析的依赖包。
    pub fn insert(&mut self, name: String, source_path: PathBuf, entry_file: PathBuf) {
        self.packages.insert(name.clone(), source_path);
        self.entries.insert(name, entry_file);
    }

    /// `import pkg;` 形式使用的入口文件。
    pub fn entry_file(&self, name: &str) -> Option<PathBuf> {
        self.entries.get(name).cloned()
    }
}
