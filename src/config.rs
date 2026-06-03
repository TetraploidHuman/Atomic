// atom.toml project configuration support
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub authors: Vec<String>,
    pub main: Option<PathBuf>,
    pub dependencies: BTreeMap<String, String>,
    pub dev_dependencies: BTreeMap<String, String>,
    pub optimize: bool,
    pub target: String,
    pub opt_level: u8,
    pub lto: bool,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "0.1.0".into(),
            description: None,
            authors: Vec::new(),
            main: None,
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
            optimize: false,
            target: "native".into(),
            opt_level: 0,
            lto: false,
        }
    }
}

impl ProjectConfig {
    /// Find and load atom.toml by walking up from the given directory.
    pub fn find_and_load(source_dir: &Path) -> Option<Self> {
        let mut dir = if source_dir.is_dir() {
            source_dir.to_path_buf()
        } else {
            source_dir.parent()?.to_path_buf()
        };

        loop {
            let candidate = dir.join("atom.toml");
            if candidate.exists() {
                match Self::load(&candidate) {
                    Ok(config) => return Some(config),
                    Err(e) => {
                        eprintln!("Warning: failed to parse {}: {}", candidate.display(), e);
                        return None;
                    }
                }
            }
            if !dir.pop() {
                break;
            }
        }
        None
    }

    /// Load and parse an atom.toml file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        Self::parse(&content, path)
    }

    /// Parse atom.toml content.
    fn parse(content: &str, _path: &Path) -> Result<Self, String> {
        let root: toml::Table = toml::from_str(content)
            .map_err(|e| format!("TOML parse error: {}", e))?;

        let mut config = Self::default();

        // [project]
        if let Some(project) = root.get("project").and_then(|v| v.as_table()) {
            config.name = project.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            config.version = project.get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("0.1.0")
                .to_string();
            config.description = project.get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            config.authors = project.get("authors")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|a| a.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            config.main = project.get("main")
                .and_then(|v| v.as_str())
                .map(|s| PathBuf::from(s));
        }

        // [dependencies]
        if let Some(deps) = root.get("dependencies").and_then(|v| v.as_table()) {
            for (name, ver) in deps {
                if let Some(v) = ver.as_str() {
                    config.dependencies.insert(name.clone(), v.to_string());
                }
            }
        }

        // [dev-dependencies]
        if let Some(deps) = root.get("dev-dependencies").and_then(|v| v.as_table()) {
            for (name, ver) in deps {
                if let Some(v) = ver.as_str() {
                    config.dev_dependencies.insert(name.clone(), v.to_string());
                }
            }
        }

        // [build]
        if let Some(build) = root.get("build").and_then(|v| v.as_table()) {
            config.optimize = build.get("optimize")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            config.target = build.get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("native")
                .to_string();
        }

        // [profile.release]
        if let Some(profile) = root.get("profile").and_then(|v| v.as_table()) {
            if let Some(release) = profile.get("release").and_then(|v| v.as_table()) {
                config.opt_level = release.get("opt_level")
                    .and_then(|v| v.as_integer())
                    .map(|i| i as u8)
                    .unwrap_or(config.opt_level);
                config.lto = release.get("lto")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
        }

        Ok(config)
    }

    /// Get the effective optimization level, merging CLI flag with config.
    pub fn effective_opt_level(&self, cli_opt: u8) -> u8 {
        // CLI flag takes precedence if explicitly set (non-zero),
        // otherwise use config from [profile.release] or [build].optimize
        if cli_opt > 0 {
            cli_opt
        } else if self.opt_level > 0 {
            self.opt_level
        } else if self.optimize {
            2 // default release optimization level
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal() {
        let toml_str = r#"
[project]
name = "test"
version = "0.1.0"
"#;
        let config = ProjectConfig::parse(toml_str, Path::new("atom.toml")).unwrap();
        assert_eq!(config.name, "test");
        assert_eq!(config.version, "0.1.0");
        assert_eq!(config.opt_level, 0);
        assert!(!config.optimize);
    }

    #[test]
    fn test_parse_full() {
        let toml_str = r#"
[project]
name = "my_project"
version = "1.2.3"
description = "A test project"
authors = ["Alice <a@example.com>", "Bob <b@example.com>"]
main = "src/main.atom"

[dependencies]
json = "1.0.0"
http = "0.2.0"

[dev-dependencies]
test = "1.0.0"

[build]
optimize = true
target = "wasm"

[profile.release]
opt_level = 3
lto = true
"#;
        let config = ProjectConfig::parse(toml_str, Path::new("atom.toml")).unwrap();
        assert_eq!(config.name, "my_project");
        assert_eq!(config.version, "1.2.3");
        assert_eq!(config.description.as_deref(), Some("A test project"));
        assert_eq!(config.authors.len(), 2);
        assert_eq!(config.main.as_ref().map(|p| p.to_str().unwrap()), Some("src/main.atom"));
        assert_eq!(config.dependencies.len(), 2);
        assert_eq!(config.dependencies.get("json").map(|s| s.as_str()), Some("1.0.0"));
        assert_eq!(config.dev_dependencies.get("test").map(|s| s.as_str()), Some("1.0.0"));
        assert!(config.optimize);
        assert_eq!(config.target, "wasm");
        assert_eq!(config.opt_level, 3);
        assert!(config.lto);
    }

    #[test]
    fn test_effective_opt_level() {
        let mut config = ProjectConfig::default();

        // CLI opt takes precedence
        assert_eq!(config.effective_opt_level(3), 3);

        // Config opt_level used when CLI is 0
        config.opt_level = 2;
        assert_eq!(config.effective_opt_level(0), 2);

        // Config optimize flag gives default 2
        config.opt_level = 0;
        config.optimize = true;
        assert_eq!(config.effective_opt_level(0), 2);

        // CLI still takes precedence
        assert_eq!(config.effective_opt_level(1), 1);
    }
}
