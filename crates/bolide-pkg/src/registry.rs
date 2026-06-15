use serde::{Deserialize, Serialize};
use std::path::{Path};

use crate::cache;
use crate::manifest::DependencySpec;

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    pub versions: Vec<IndexVersion>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexVersion {
    pub version: String,
    pub checksum: String,
    pub download_url: String,
}

/// 按 Cargo-style 分层路径构造 index URL。
fn index_url(registry: &str, name: &str) -> String {
    let lower = name.to_lowercase();
    let prefix = if lower.len() >= 3 {
        format!("{}/{}/{}", &lower[..2], &lower[2..3], lower)
    } else {
        format!("{}/{}", &lower[..1], lower)
    };
    format!("{}/{}/{}.json", registry.trim_end_matches('/'), prefix, name)
}

pub fn fetch_index(registry: &str, name: &str) -> Result<IndexEntry, String> {
    let url = index_url(registry, name);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("Failed to fetch index for '{}': {}", name, e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Registry returned {} for '{}': {}",
            response.status(),
            name,
            url
        ));
    }

    response
        .json()
        .map_err(|e| format!("Failed to parse index for '{}': {}", name, e))
}

pub fn resolve_registry_dep(
    name: &str,
    version: &str,
    registry: &str,
) -> Result<crate::resolve::ResolvedDep, String> {
    let index = fetch_index(registry, name)?;
    let matched = index
        .versions
        .into_iter()
        .find(|v| v.version == version)
        .ok_or_else(|| format!("Version '{}' of '{}' not found in registry", version, name))?;

    let registry_host = registry_host(registry);
    let dest = cache::registry_cache_path(&registry_host, name, version);

    if !dest.exists() {
        cache::ensure_dir(&dest)?;
        download_and_extract(&matched.download_url,
            &dest,
            &matched.checksum,
        )?;
    }

    entry_from_source(name, DependencySpec::Registry { version: version.to_string(), registry: Some(registry.to_string()) }, &dest)
}

fn registry_host(registry: &str) -> String {
    registry
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

fn download_and_extract(url: &str, dest: &Path, checksum: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let bytes = client
        .get(url)
        .send()
        .map_err(|e| format!("Failed to download '{}': {}", url, e))?
        .bytes()
        .map_err(|e| format!("Failed to read download body: {}", e))?;

    // 第一阶段：仅 warning 级别 checksum 校验
    let computed = format!("sha256:{}", sha256(&bytes));
    if computed != checksum {
        eprintln!(
            "Warning: checksum mismatch for {}. Expected {}, got {}.",
            url, checksum, computed
        );
    }

    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        extract_tar(&bytes, dest)?;
    } else if url.ends_with(".zip") {
        extract_zip(&bytes, dest)?;
    } else {
        return Err(format!("Unsupported archive format: {}", url));
    }

    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // 第一阶段用 std hash 占位；生产应换 ring/sha2
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn extract_tar(bytes: &[u8], dest: &Path) -> Result<(), String> {
    #[cfg(feature = "archive")]
    {
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);
        archive
            .unpack(dest)
            .map_err(|e| format!("Failed to extract tar archive: {}", e))
    }
    #[cfg(not(feature = "archive"))]
    {
        let _ = (bytes, dest);
        Err("Archive extraction is not enabled in this build".to_string())
    }
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), String> {
    #[cfg(feature = "archive")]
    {
        let reader = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|e| format!("Failed to read zip archive: {}", e))?;
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| format!("Failed to access zip entry: {}", e))?;
            let out_path = dest.join(file.name());
            if file.is_dir() {
                std::fs::create_dir_all(&out_path)
                    .map_err(|e| format!("Failed to create zip dir '{}': {}", out_path.display(), e))?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create zip parent '{}': {}", parent.display(), e))?;
                }
                let mut out = std::fs::File::create(&out_path)
                    .map_err(|e| format!("Failed to create zip file '{}': {}", out_path.display(), e))?;
                std::io::copy(&mut file, &mut out)
                    .map_err(|e| format!("Failed to write zip entry: {}", e))?;
            }
        }
        Ok(())
    }
    #[cfg(not(feature = "archive"))]
    {
        let _ = (bytes, dest);
        Err("Archive extraction is not enabled in this build".to_string())
    }
}

fn entry_from_source(name: &str, spec: DependencySpec, source_path: &Path) -> Result<crate::resolve::ResolvedDep, String> {
    let manifest = crate::manifest::parse_manifest(&source_path.join("bolide.toml"))
        .map_err(|e| format!("Dependency '{}' is missing or has invalid bolide.toml: {}", name, e))?;
    let entry = source_path.join(&manifest.package.lib);
    if !entry.exists() {
        return Err(format!(
            "Dependency '{}' entry file not found: {}",
            name,
            entry.display()
        ));
    }
    Ok(crate::resolve::ResolvedDep {
        name: name.to_string(),
        spec,
        source_path: source_path.to_path_buf(),
        entry_file: entry,
    })
}

// tar/zip 依赖：通过 feature "archive" 启用，减少默认编译依赖。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_url() {
        assert_eq!(
            index_url("https://registry.bolide.dev", "http"),
            "https://registry.bolide.dev/ht/t/http/http.json"
        );
    }
}
