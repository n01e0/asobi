use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub model: Option<String>,
    pub region: Option<String>,
    pub system_prompt: Option<String>,
    #[allow(dead_code)]
    pub provider: Option<ProviderConfig>,
    #[serde(default)]
    pub tools: Vec<WasmToolConfig>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProviderConfig {
    #[allow(dead_code)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WasmToolConfig {
    pub name: String,
    pub wasm: String,
    pub description: Option<String>,
    #[serde(default)]
    pub permissions: WasmPermissions,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WasmPermissions {
    #[serde(default)]
    pub fs_read: Vec<String>,
    #[serde(default)]
    pub fs_write: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(|xdg| PathBuf::from(xdg).join("asobi"))
        .or_else(|| home::home_dir().map(|h| h.join(".asobi")))
}

pub fn load() -> Config {
    let Some(dir) = config_dir() else {
        return Config::default();
    };
    let path = dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    toml::from_str(&content).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
model = "anthropic.claude-sonnet-4-20250514-v1:0"
region = "us-west-2"
system_prompt = "You are a Rust expert."

[provider]
name = "bedrock"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.model.as_deref(), Some("anthropic.claude-sonnet-4-20250514-v1:0"));
        assert_eq!(config.region.as_deref(), Some("us-west-2"));
        assert_eq!(config.system_prompt.as_deref(), Some("You are a Rust expert."));
        assert_eq!(config.provider.unwrap().name.as_deref(), Some("bedrock"));
    }

    #[test]
    fn test_parse_partial_config() {
        let toml = r#"
model = "openai.gpt-oss-120b-1:0"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.model.as_deref(), Some("openai.gpt-oss-120b-1:0"));
        assert!(config.region.is_none());
        assert!(config.tools.is_empty());
    }

    #[test]
    fn test_parse_empty_config() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.model.is_none());
    }

    #[test]
    fn test_parse_wasm_tools() {
        let toml = r#"
[[tools]]
name = "search_code"
wasm = "plugins/search_code.wasm"
description = "Search code using ripgrep"

[tools.permissions]
fs_read = ["."]
env = ["PATH"]

[[tools]]
name = "fetch_url"
wasm = "plugins/fetch_url.wasm"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.tools.len(), 2);
        assert_eq!(config.tools[0].name, "search_code");
        assert_eq!(config.tools[0].permissions.fs_read, vec!["."]);
        assert_eq!(config.tools[0].permissions.env, vec!["PATH"]);
        assert_eq!(config.tools[1].name, "fetch_url");
        assert!(config.tools[1].permissions.fs_read.is_empty());
    }

    #[test]
    fn test_load_missing_file() {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "/nonexistent/xdg/path") };
        let config = load();
        assert!(config.model.is_none());
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
}
