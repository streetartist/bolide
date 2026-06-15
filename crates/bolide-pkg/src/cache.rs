use std::path::PathBuf;

/// 返回平台相关的 Bolide 缓存根目录。
pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BOLIDE_CACHE_DIR") {
        return PathBuf::from(dir);
    }

    #[cfg(target_os = "windows")]
    {
        let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
            format!("{}\\Local", home)
        });
        PathBuf::from(local_app_data).join("bolide")
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".cache").join("bolide")
    }
}

pub fn packages_cache_dir() -> PathBuf {
    cache_dir().join("packages")
}

pub fn index_cache_dir() -> PathBuf {
    cache_dir().join("index")
}

pub fn git_cache_path(host: &str, owner: &str, repo: &str, ref_: &str) -> PathBuf {
    packages_cache_dir()
        .join(host)
        .join(owner)
        .join(repo)
        .join(ref_)
}

pub fn registry_cache_path(registry_host: &str, name: &str, version: &str) -> PathBuf {
    packages_cache_dir()
        .join("registry")
        .join(registry_host)
        .join(name)
        .join(version)
}

pub fn ensure_dir(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("Failed to create directory '{}': {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_cache_path() {
        let path = git_cache_path("github.com", "bolide-lang", "http", "v1.2.0");
        assert!(path.to_string_lossy().contains("bolide"));
        assert!(path.to_string_lossy().contains("github.com"));
    }
}
