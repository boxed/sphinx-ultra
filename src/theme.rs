//! Theme system for sphinx-ultra
//!
//! This module provides theme discovery, loading, and management for the HTML builder.
//! Themes can inherit from other themes and provide templates, static files, and options.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A theme stylesheet entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeStylesheet {
    /// Path relative to theme's static directory
    pub path: String,
    /// Loading priority (lower = earlier in document)
    #[serde(default = "default_priority")]
    pub priority: i32,
}

/// A theme script entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeScript {
    /// Path relative to theme's static directory
    pub path: String,
    /// Loading priority (lower = earlier in document)
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// Whether to use defer attribute
    #[serde(default)]
    pub defer: bool,
    /// Whether to use async attribute
    #[serde(rename = "async", default)]
    pub async_: bool,
}

fn default_priority() -> i32 {
    200
}

/// Theme option type for validation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeOptionType {
    Bool,
    String,
    Integer,
    Float,
}

/// Theme option specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeOptionSpec {
    #[serde(rename = "type")]
    pub option_type: ThemeOptionType,
    pub default: serde_json::Value,
    #[serde(default)]
    pub values: Option<Vec<String>>,
}

/// Raw theme.toml structure for deserialization
#[derive(Debug, Clone, Deserialize)]
struct ThemeToml {
    theme: ThemeTomlMeta,
}

#[derive(Debug, Clone, Deserialize)]
struct ThemeTomlMeta {
    name: String,
    #[serde(default)]
    inherit: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    stylesheets: Option<ThemeTomlAssets>,
    #[serde(default)]
    scripts: Option<ThemeTomlAssets>,
    #[serde(default)]
    options: Option<HashMap<String, ThemeOptionSpec>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ThemeTomlAssets {
    #[serde(default)]
    files: Vec<String>,
    #[serde(default = "default_priority")]
    priority: i32,
}

/// A Sphinx theme
#[derive(Debug, Clone)]
pub struct Theme {
    /// Theme name
    pub name: String,
    /// Parent theme to inherit from
    pub inherit: Option<String>,
    /// Theme version
    pub version: String,
    /// Path to theme directory
    pub path: PathBuf,
    /// Theme stylesheets
    pub stylesheets: Vec<ThemeStylesheet>,
    /// Theme scripts
    pub scripts: Vec<ThemeScript>,
    /// Theme options schema
    pub options_schema: HashMap<String, ThemeOptionSpec>,
    /// Path to templates directory (if exists)
    pub templates_dir: Option<PathBuf>,
    /// Path to static files directory (if exists)
    pub static_dir: Option<PathBuf>,
}

impl Theme {
    /// Load a theme from a directory containing theme.toml or theme.conf
    pub fn from_path(path: &Path) -> Result<Self> {
        let theme_toml_path = path.join("theme.toml");
        let theme_conf_path = path.join("theme.conf");

        if theme_toml_path.exists() {
            Self::from_toml(path, &theme_toml_path)
        } else if theme_conf_path.exists() {
            Self::from_conf(path, &theme_conf_path)
        } else {
            Err(anyhow!(
                "Theme directory {} does not contain theme.toml or theme.conf",
                path.display()
            ))
        }
    }

    /// Load a theme from theme.toml format
    fn from_toml(path: &Path, theme_toml_path: &Path) -> Result<Self> {
        if !theme_toml_path.exists() {
            return Err(anyhow!(
                "Theme file {} does not exist",
                theme_toml_path.display()
            ));
        }

        let content = std::fs::read_to_string(&theme_toml_path)
            .with_context(|| format!("Failed to read {}", theme_toml_path.display()))?;

        let toml: ThemeToml = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", theme_toml_path.display()))?;

        let meta = toml.theme;

        // Parse stylesheets
        let stylesheets = if let Some(assets) = meta.stylesheets {
            assets
                .files
                .into_iter()
                .map(|path| ThemeStylesheet {
                    path,
                    priority: assets.priority,
                })
                .collect()
        } else {
            Vec::new()
        };

        // Parse scripts
        let scripts = if let Some(assets) = meta.scripts {
            assets
                .files
                .into_iter()
                .map(|path| ThemeScript {
                    path,
                    priority: assets.priority,
                    defer: false,
                    async_: false,
                })
                .collect()
        } else {
            Vec::new()
        };

        // Check for templates and static directories
        let templates_dir = path.join("templates");
        let templates_dir = if templates_dir.is_dir() {
            Some(templates_dir)
        } else {
            None
        };

        let static_dir = path.join("static");
        let static_dir = if static_dir.is_dir() {
            Some(static_dir)
        } else {
            None
        };

        Ok(Theme {
            name: meta.name,
            inherit: meta.inherit,
            version: meta.version.unwrap_or_else(|| "0.0.0".to_string()),
            path: path.to_path_buf(),
            stylesheets,
            scripts,
            options_schema: meta.options.unwrap_or_default(),
            templates_dir,
            static_dir,
        })
    }

    /// Load a theme from Sphinx's theme.conf format (INI-style)
    fn from_conf(path: &Path, theme_conf_path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(theme_conf_path)
            .with_context(|| format!("Failed to read {}", theme_conf_path.display()))?;

        // Parse INI-style theme.conf
        // Format:
        // [theme]
        // inherit = basic
        // stylesheet = theme.css
        // pygments_style = sphinx
        //
        // [options]
        // option_name = default_value

        let mut inherit: Option<String> = None;
        let mut stylesheets: Vec<ThemeStylesheet> = Vec::new();
        let mut options_schema: HashMap<String, ThemeOptionSpec> = HashMap::new();
        let mut current_section = String::new();

        for line in content.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            // Section header
            if line.starts_with('[') && line.ends_with(']') {
                current_section = line[1..line.len() - 1].to_lowercase();
                continue;
            }

            // Key = value
            if let Some(eq_pos) = line.find('=') {
                let key = line[..eq_pos].trim().to_lowercase();
                let value = line[eq_pos + 1..].trim();

                match current_section.as_str() {
                    "theme" => {
                        match key.as_str() {
                            "inherit" => {
                                if value != "none" && !value.is_empty() {
                                    inherit = Some(value.to_string());
                                }
                            }
                            "stylesheet" => {
                                // Can be comma-separated list
                                for css in value.split(',') {
                                    let css = css.trim();
                                    if !css.is_empty() {
                                        stylesheets.push(ThemeStylesheet {
                                            path: css.to_string(),
                                            priority: 200,
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    "options" => {
                        // All options in theme.conf are strings with their default values
                        options_schema.insert(
                            key,
                            ThemeOptionSpec {
                                option_type: ThemeOptionType::String,
                                default: serde_json::Value::String(value.to_string()),
                                values: None,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }

        // Derive theme name from directory name
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Check for templates and static directories
        let templates_dir = path.join("templates");
        let templates_dir = if templates_dir.is_dir() {
            Some(templates_dir)
        } else {
            None
        };

        let static_dir = path.join("static");
        let static_dir = if static_dir.is_dir() {
            Some(static_dir)
        } else {
            None
        };

        Ok(Theme {
            name,
            inherit,
            version: "0.0.0".to_string(),
            path: path.to_path_buf(),
            stylesheets,
            scripts: Vec::new(), // theme.conf doesn't specify scripts
            options_schema,
            templates_dir,
            static_dir,
        })
    }

    /// Get effective options by merging user options with defaults
    pub fn get_effective_options(&self, user_options: &serde_json::Value) -> serde_json::Value {
        let mut result = serde_json::Map::new();

        // Start with defaults from schema
        for (key, spec) in &self.options_schema {
            result.insert(key.clone(), spec.default.clone());
        }

        // Override with user options
        if let serde_json::Value::Object(user_map) = user_options {
            for (key, value) in user_map {
                result.insert(key.clone(), value.clone());
            }
        }

        serde_json::Value::Object(result)
    }

    /// Validate user options against the schema
    pub fn validate_options(&self, user_options: &serde_json::Value) -> Result<()> {
        if let serde_json::Value::Object(user_map) = user_options {
            for (key, value) in user_map {
                if let Some(spec) = self.options_schema.get(key) {
                    // Validate type
                    let valid = match spec.option_type {
                        ThemeOptionType::Bool => value.is_boolean(),
                        ThemeOptionType::String => value.is_string(),
                        ThemeOptionType::Integer => value.is_i64(),
                        ThemeOptionType::Float => value.is_f64() || value.is_i64(),
                    };

                    if !valid {
                        return Err(anyhow!(
                            "Theme option '{}' has invalid type, expected {:?}",
                            key,
                            spec.option_type
                        ));
                    }

                    // Validate allowed values if specified
                    if let Some(allowed) = &spec.values {
                        if let Some(s) = value.as_str() {
                            if !allowed.contains(&s.to_string()) {
                                return Err(anyhow!(
                                    "Theme option '{}' has invalid value '{}', allowed: {:?}",
                                    key,
                                    s,
                                    allowed
                                ));
                            }
                        }
                    }
                }
                // Unknown options are allowed (for forward compatibility)
            }
        }

        Ok(())
    }
}

/// Registry for discovering and managing themes
#[derive(Debug, Default)]
pub struct ThemeRegistry {
    /// Registered themes by name
    themes: HashMap<String, Theme>,
    /// Directories to search for themes
    search_paths: Vec<PathBuf>,
}

impl ThemeRegistry {
    /// Create a new empty theme registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a search path for theme discovery
    pub fn add_search_path(&mut self, path: PathBuf) {
        if !self.search_paths.contains(&path) {
            self.search_paths.push(path);
        }
    }

    /// Discover themes in all search paths
    pub fn discover_themes(&mut self) -> Result<()> {
        for search_path in &self.search_paths.clone() {
            if !search_path.exists() {
                continue;
            }

            // Each subdirectory with a theme.toml or theme.conf is a theme
            if let Ok(entries) = std::fs::read_dir(search_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir()
                        && (path.join("theme.toml").exists() || path.join("theme.conf").exists())
                    {
                        match Theme::from_path(&path) {
                            Ok(theme) => {
                                log::debug!(
                                    "Discovered theme: {} at {}",
                                    theme.name,
                                    path.display()
                                );
                                self.themes.insert(theme.name.clone(), theme);
                            }
                            Err(e) => {
                                log::warn!(
                                    "Failed to load theme from {}: {}",
                                    path.display(),
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Try to find a specific theme installed via pip in the Python environment
    ///
    /// This calls out to Python to find the installed path of the requested theme.
    pub fn  discover_python_theme(&mut self, theme_name: &str) -> Result<bool> {
        // Check if already registered
        if self.themes.contains_key(theme_name) {
            return Ok(true);
        }

        // Python script to find a specific theme
        let python_script = format!(
            r#"
import sys
import os

theme_name = "{}"

# Try direct import first
try:
    module = __import__(theme_name)
    print(f"DEBUG: Imported {{theme_name}}, module={{module}}", file=sys.stderr)
    print(f"DEBUG: dir(module)={{dir(module)}}", file=sys.stderr)

    # Many Sphinx themes provide get_html_theme_path()
    if hasattr(module, 'get_html_theme_path'):
        theme_path = module.get_html_theme_path()
        print(f"DEBUG: get_html_theme_path() returned {{theme_path}}", file=sys.stderr)
        if isinstance(theme_path, (list, tuple)):
            theme_path = theme_path[0]
        print(theme_path)
        sys.exit(0)

    if hasattr(module, '__path__'):
        print(f"DEBUG: __path__={{module.__path__}}", file=sys.stderr)
        print(module.__path__[0])
        sys.exit(0)
    elif hasattr(module, '__file__'):
        print(f"DEBUG: __file__={{module.__file__}}", file=sys.stderr)
        print(os.path.dirname(module.__file__))
        sys.exit(0)
    else:
        print(f"DEBUG: No __path__ or __file__ found", file=sys.stderr)
except ImportError as e:
    print(f"DEBUG: ImportError: {{e}}", file=sys.stderr)
except Exception as e:
    print(f"DEBUG: Exception during import: {{e}}", file=sys.stderr)

# Try via entry points (Python 3.9+)
try:
    from importlib.metadata import entry_points
    eps = entry_points()
    if hasattr(eps, 'select'):
        # Python 3.10+
        sphinx_themes = eps.select(group='sphinx.html_themes')
    else:
        # Python 3.9
        sphinx_themes = eps.get('sphinx.html_themes', [])

    print(f"DEBUG: Found entry points: {{list(sphinx_themes)}}", file=sys.stderr)
    for ep in sphinx_themes:
        if ep.name == theme_name:
            print(f"DEBUG: Found matching entry point: {{ep}}", file=sys.stderr)
            theme_module = ep.load()
            if hasattr(theme_module, '__path__'):
                print(theme_module.__path__[0])
                sys.exit(0)
            elif hasattr(theme_module, '__file__'):
                print(os.path.dirname(theme_module.__file__))
                sys.exit(0)
except Exception as e:
    print(f"DEBUG: Entry points error: {{e}}", file=sys.stderr)

print(f"DEBUG: Could not find theme path for {{theme_name}}", file=sys.stderr)
sys.exit(1)
"#,
            theme_name
        );

        // Try python3 first, then python
        let python_cmd = if Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            "python3"
        } else {
            "python"
        };

        // Get and log the full path of the Python executable
        if let Ok(output) = Command::new(python_cmd)
            .arg("-c")
            .arg("import sys; print(sys.executable)")
            .output()
        {
            if output.status.success() {
                let python_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                log::info!("Using Python: {}", python_path);
            }
        }

        let output = match Command::new(python_cmd)
            .arg("-c")
            .arg(&python_script)
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                log::debug!("Failed to run Python for theme discovery: {}", e);
                return Ok(false);
            }
        };

        // Log stderr for debugging
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            for line in stderr.lines() {
                log::info!("{}", line);
            }
        }

        if !output.status.success() {
            return Ok(false);
        }

        let theme_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        log::info!("Python returned theme path: {}", theme_path);
        let path = PathBuf::from(&theme_path);

        if !path.is_dir() {
            log::info!("Theme path is not a directory: {}", path.display());
            return Ok(false);
        }

        // Try multiple possible locations for theme.conf:
        // 1. Directly in the package directory (e.g., sphinx_rtd_theme/)
        // 2. In a theme/<theme_name>/ subdirectory (e.g., furo/theme/furo/)
        // 3. In a <theme_name>/ subdirectory (e.g., package/theme_name/)
        let possible_paths = [
            path.clone(),
            path.join("theme").join(theme_name),
            path.join(theme_name),
        ];

        for theme_dir in &possible_paths {
            if !theme_dir.is_dir() {
                continue;
            }

            let has_theme_toml = theme_dir.join("theme.toml").exists();
            let has_theme_conf = theme_dir.join("theme.conf").exists();

            if has_theme_toml || has_theme_conf {
                log::info!(
                    "Found theme config at {} - theme.toml: {}, theme.conf: {}",
                    theme_dir.display(),
                    has_theme_toml,
                    has_theme_conf
                );

                match Theme::from_path(theme_dir) {
                    Ok(theme) => {
                        log::info!(
                            "Found Python-installed theme '{}' at {}",
                            theme.name,
                            theme_dir.display()
                        );
                        self.themes.insert(theme.name.clone(), theme);
                        return Ok(true);
                    }
                    Err(e) => {
                        log::info!("Failed to load Python theme from {}: {}", theme_dir.display(), e);
                    }
                }
            }
        }

        log::info!("No theme.toml or theme.conf found in any expected location");
        Ok(false)
    }

    /// Register a theme directly
    pub fn register(&mut self, theme: Theme) {
        self.themes.insert(theme.name.clone(), theme);
    }

    /// Get a theme by name
    pub fn get_theme(&self, name: &str) -> Option<&Theme> {
        self.themes.get(name)
    }

    /// Check if a theme exists
    pub fn has_theme(&self, name: &str) -> bool {
        self.themes.contains_key(name)
    }

    /// Get all registered theme names
    pub fn theme_names(&self) -> Vec<&str> {
        self.themes.keys().map(|s| s.as_str()).collect()
    }

    /// Resolve the inheritance chain for a theme
    /// Returns themes from root ancestor to the requested theme
    pub fn resolve_theme_chain(&self, name: &str) -> Result<Vec<&Theme>> {
        let mut chain = Vec::new();
        let mut current_name = name;
        let mut seen = std::collections::HashSet::new();

        loop {
            if seen.contains(current_name) {
                return Err(anyhow!(
                    "Circular theme inheritance detected: {}",
                    current_name
                ));
            }
            seen.insert(current_name.to_string());

            let theme = self.get_theme(current_name).ok_or_else(|| {
                anyhow!("Theme '{}' not found in registry", current_name)
            })?;

            chain.push(theme);

            if let Some(ref parent) = theme.inherit {
                current_name = parent;
            } else {
                break;
            }
        }

        // Reverse so root ancestor is first
        chain.reverse();
        Ok(chain)
    }

    /// Get merged options for a theme chain
    /// Options from child themes override parent themes
    pub fn get_merged_options(
        &self,
        name: &str,
        user_options: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let chain = self.resolve_theme_chain(name)?;
        let mut merged = serde_json::Map::new();

        // Merge defaults from root to leaf
        for theme in &chain {
            for (key, spec) in &theme.options_schema {
                merged.insert(key.clone(), spec.default.clone());
            }
        }

        // Override with user options
        if let serde_json::Value::Object(user_map) = user_options {
            for (key, value) in user_map {
                merged.insert(key.clone(), value.clone());
            }
        }

        Ok(serde_json::Value::Object(merged))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_theme(dir: &Path, name: &str, inherit: Option<&str>) -> Result<()> {
        let theme_dir = dir.join(name);
        std::fs::create_dir_all(&theme_dir)?;

        let inherit_line = inherit
            .map(|p| format!("inherit = \"{}\"", p))
            .unwrap_or_default();

        let theme_toml = format!(
            r#"
[theme]
name = "{}"
{}
version = "1.0.0"

[theme.stylesheets]
files = ["{}.css"]
priority = 200

[theme.options]
test_option = {{ type = "bool", default = false }}
"#,
            name, inherit_line, name
        );

        let mut file = std::fs::File::create(theme_dir.join("theme.toml"))?;
        file.write_all(theme_toml.as_bytes())?;

        // Create static and templates dirs
        std::fs::create_dir_all(theme_dir.join("static"))?;
        std::fs::create_dir_all(theme_dir.join("templates"))?;

        Ok(())
    }

    #[test]
    fn test_theme_from_path() {
        let temp_dir = TempDir::new().unwrap();
        create_test_theme(temp_dir.path(), "test-theme", None).unwrap();

        let theme = Theme::from_path(&temp_dir.path().join("test-theme")).unwrap();
        assert_eq!(theme.name, "test-theme");
        assert!(theme.inherit.is_none());
        assert_eq!(theme.stylesheets.len(), 1);
        assert_eq!(theme.stylesheets[0].path, "test-theme.css");
        assert!(theme.templates_dir.is_some());
        assert!(theme.static_dir.is_some());
    }

    #[test]
    fn test_theme_inheritance() {
        let temp_dir = TempDir::new().unwrap();
        create_test_theme(temp_dir.path(), "base", None).unwrap();
        create_test_theme(temp_dir.path(), "child", Some("base")).unwrap();

        let mut registry = ThemeRegistry::new();
        registry.add_search_path(temp_dir.path().to_path_buf());
        registry.discover_themes().unwrap();

        let chain = registry.resolve_theme_chain("child").unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].name, "base");
        assert_eq!(chain[1].name, "child");
    }

    #[test]
    fn test_circular_inheritance_detection() {
        let temp_dir = TempDir::new().unwrap();

        // Create theme A that inherits from B
        let theme_a_dir = temp_dir.path().join("theme-a");
        std::fs::create_dir_all(&theme_a_dir).unwrap();
        std::fs::write(
            theme_a_dir.join("theme.toml"),
            r#"
[theme]
name = "theme-a"
inherit = "theme-b"
"#,
        )
        .unwrap();

        // Create theme B that inherits from A (circular!)
        let theme_b_dir = temp_dir.path().join("theme-b");
        std::fs::create_dir_all(&theme_b_dir).unwrap();
        std::fs::write(
            theme_b_dir.join("theme.toml"),
            r#"
[theme]
name = "theme-b"
inherit = "theme-a"
"#,
        )
        .unwrap();

        let mut registry = ThemeRegistry::new();
        registry.add_search_path(temp_dir.path().to_path_buf());
        registry.discover_themes().unwrap();

        let result = registry.resolve_theme_chain("theme-a");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Circular"));
    }

    #[test]
    fn test_theme_options() {
        let temp_dir = TempDir::new().unwrap();
        create_test_theme(temp_dir.path(), "test-theme", None).unwrap();

        let theme = Theme::from_path(&temp_dir.path().join("test-theme")).unwrap();

        // Test default options
        let defaults = theme.get_effective_options(&serde_json::json!({}));
        assert_eq!(defaults["test_option"], false);

        // Test user override
        let with_override = theme.get_effective_options(&serde_json::json!({
            "test_option": true
        }));
        assert_eq!(with_override["test_option"], true);
    }

    #[test]
    fn test_theme_option_validation() {
        let temp_dir = TempDir::new().unwrap();
        create_test_theme(temp_dir.path(), "test-theme", None).unwrap();

        let theme = Theme::from_path(&temp_dir.path().join("test-theme")).unwrap();

        // Valid option
        assert!(theme
            .validate_options(&serde_json::json!({"test_option": true}))
            .is_ok());

        // Invalid type (string instead of bool)
        assert!(theme
            .validate_options(&serde_json::json!({"test_option": "yes"}))
            .is_err());
    }

    #[test]
    fn test_theme_from_conf() {
        let temp_dir = TempDir::new().unwrap();
        let theme_dir = temp_dir.path().join("sphinx_rtd_theme");
        std::fs::create_dir_all(&theme_dir).unwrap();
        std::fs::create_dir_all(theme_dir.join("static")).unwrap();
        std::fs::create_dir_all(theme_dir.join("templates")).unwrap();

        // Create a Sphinx-style theme.conf
        std::fs::write(
            theme_dir.join("theme.conf"),
            r#"
[theme]
inherit = basic
stylesheet = css/theme.css
pygments_style = sphinx

[options]
logo_only = false
display_version = true
style_nav_header_background = #2980B9
"#,
        )
        .unwrap();

        let theme = Theme::from_path(&theme_dir).unwrap();
        assert_eq!(theme.name, "sphinx_rtd_theme");
        assert_eq!(theme.inherit, Some("basic".to_string()));
        assert_eq!(theme.stylesheets.len(), 1);
        assert_eq!(theme.stylesheets[0].path, "css/theme.css");
        assert!(theme.options_schema.contains_key("logo_only"));
        assert!(theme.options_schema.contains_key("display_version"));
        assert!(theme.options_schema.contains_key("style_nav_header_background"));
    }

    #[test]
    fn test_theme_conf_multiple_stylesheets() {
        let temp_dir = TempDir::new().unwrap();
        let theme_dir = temp_dir.path().join("multi_css");
        std::fs::create_dir_all(&theme_dir).unwrap();

        std::fs::write(
            theme_dir.join("theme.conf"),
            r#"
[theme]
inherit = none
stylesheet = base.css, theme.css, extra.css
"#,
        )
        .unwrap();

        let theme = Theme::from_path(&theme_dir).unwrap();
        assert!(theme.inherit.is_none());
        assert_eq!(theme.stylesheets.len(), 3);
        assert_eq!(theme.stylesheets[0].path, "base.css");
        assert_eq!(theme.stylesheets[1].path, "theme.css");
        assert_eq!(theme.stylesheets[2].path, "extra.css");
    }
}
