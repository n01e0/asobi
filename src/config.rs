use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub model: Option<String>,
    pub region: Option<String>,
    pub system_prompt: Option<String>,
    #[allow(dead_code)]
    pub provider: Option<ProviderConfig>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub tools: Vec<WasmToolConfig>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProviderConfig {
    #[allow(dead_code)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Permissions {
    #[serde(default = "default_fs_all")]
    pub fs_read: Vec<String>,
    #[serde(default = "default_fs_all")]
    pub fs_write: Vec<String>,
    #[serde(default = "default_commands_allow")]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub denied_commands: Vec<String>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            fs_read: default_fs_all(),
            fs_write: default_fs_all(),
            allowed_commands: default_commands_allow(),
            denied_commands: Vec::new(),
        }
    }
}

fn default_fs_all() -> Vec<String> {
    vec!["/".to_string()]
}

fn default_commands_allow() -> Vec<String> {
    vec!["*".to_string()]
}

impl Permissions {
    pub fn intersect(&self, local: &Permissions) -> Permissions {
        Permissions {
            fs_read: intersect_paths(&self.fs_read, &local.fs_read),
            fs_write: intersect_paths(&self.fs_write, &local.fs_write),
            allowed_commands: if local.allowed_commands == ["*"] {
                self.allowed_commands.clone()
            } else {
                local.allowed_commands.clone()
            },
            denied_commands: {
                let mut denied = self.denied_commands.clone();
                for d in &local.denied_commands {
                    if !denied.contains(d) {
                        denied.push(d.clone());
                    }
                }
                denied
            },
        }
    }

    pub fn is_path_readable(&self, path: &str) -> bool {
        check_path_allowed(&self.fs_read, path)
    }

    pub fn is_path_writable(&self, path: &str) -> bool {
        check_path_allowed(&self.fs_write, path)
    }

    pub fn is_command_allowed(&self, command: &str) -> bool {
        for denied in &self.denied_commands {
            if command.contains(denied) {
                return false;
            }
        }
        if self.allowed_commands.contains(&"*".to_string()) {
            return true;
        }
        let cmd_name = command.split_whitespace().next().unwrap_or("");
        self.allowed_commands.iter().any(|a| a == cmd_name)
    }
}

fn check_path_allowed(allowed: &[String], path: &str) -> bool {
    if allowed.iter().any(|a| a == "/") {
        return true;
    }
    let target = std::path::absolute(Path::new(path))
        .unwrap_or_else(|_| PathBuf::from(path));
    for allowed_path in allowed {
        let base = std::path::absolute(Path::new(allowed_path))
            .unwrap_or_else(|_| PathBuf::from(allowed_path));
        if target.starts_with(&base) {
            return true;
        }
    }
    false
}

fn intersect_paths(global: &[String], local: &[String]) -> Vec<String> {
    if global.iter().any(|g| g == "/") {
        return local.to_vec();
    }
    if local.iter().any(|l| l == "/") {
        return global.to_vec();
    }
    let mut result = Vec::new();
    for l in local {
        let l_abs = std::path::absolute(Path::new(l))
            .unwrap_or_else(|_| PathBuf::from(l));
        for g in global {
            let g_abs = std::path::absolute(Path::new(g))
                .unwrap_or_else(|_| PathBuf::from(g));
            if l_abs.starts_with(&g_abs) {
                result.push(l.clone());
                break;
            }
        }
    }
    result
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
    pub mounts: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub host_path: String,
    pub guest_path: String,
    pub writable: bool,
}

impl WasmPermissions {
    pub fn resolved_mounts(&self) -> Vec<Mount> {
        let mut mounts = Vec::new();
        for spec in &self.mounts {
            let (host, guest, writable) = parse_mount_spec(spec);
            mounts.push(Mount {
                host_path: host,
                guest_path: guest,
                writable,
            });
        }
        if mounts.is_empty() {
            mounts.push(Mount {
                host_path: ".".to_string(),
                guest_path: ".".to_string(),
                writable: true,
            });
        }
        mounts
    }
}

fn parse_mount_spec(spec: &str) -> (String, String, bool) {
    let (spec, writable) = if let Some(s) = spec.strip_suffix(":ro") {
        (s, false)
    } else {
        (spec, true)
    };
    if let Some((host, guest)) = spec.split_once(':') {
        (host.to_string(), guest.to_string(), writable)
    } else {
        (spec.to_string(), spec.to_string(), writable)
    }
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(|xdg| PathBuf::from(xdg).join("asobi"))
        .or_else(|| home::home_dir().map(|h| h.join(".asobi")))
}

pub fn load() -> Result<Config, String> {
    let Some(dir) = config_dir() else {
        return Ok(Config::default());
    };
    let path = dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(Config::default());
    };
    toml::from_str(&content).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load_local() -> Result<Option<Config>, String> {
    let path = Path::new(".asobi/config.toml");
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    toml::from_str(&content)
        .map(Some)
        .map_err(|e| format!(".asobi/config.toml: {e}"))
}

pub fn load_merged() -> Result<(Config, Permissions, Vec<WasmToolConfig>), String> {
    let global = load()?;
    let mut perms = global.permissions.clone();
    let mut tools = global.tools.clone();

    if let Some(local) = load_local()? {
        perms = perms.intersect(&local.permissions);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        for mut tool in local.tools {
            if !Path::new(&tool.wasm).is_absolute() {
                tool.wasm = cwd.join(&tool.wasm).to_string_lossy().to_string();
            }
            if !tools.iter().any(|t| t.name == tool.name) {
                tools.push(tool);
            }
        }
    }

    Ok((global, perms, tools))
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

[permissions]
fs_read = ["/home", "/tmp"]
fs_write = ["."]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.model.as_deref(), Some("anthropic.claude-sonnet-4-20250514-v1:0"));
        assert_eq!(config.permissions.fs_read, vec!["/home", "/tmp"]);
        assert_eq!(config.permissions.fs_write, vec!["."]);
    }

    #[test]
    fn test_default_permissions() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.permissions.fs_read, vec!["/"]);
        assert_eq!(config.permissions.fs_write, vec!["/"]);
        assert_eq!(config.permissions.allowed_commands, vec!["*"]);
    }

    #[test]
    fn test_intersect_restricts() {
        let global = Permissions {
            fs_read: vec!["/".to_string()],
            fs_write: vec!["/home".to_string()],
            allowed_commands: vec!["*".to_string()],
            denied_commands: vec![],
        };
        let local = Permissions {
            fs_read: vec![".".to_string()],
            fs_write: vec![".".to_string()],
            allowed_commands: vec!["*".to_string()],
            denied_commands: vec!["rm -rf".to_string()],
        };
        let merged = global.intersect(&local);
        assert_eq!(merged.fs_read, vec!["."]);
        assert!(merged.denied_commands.contains(&"rm -rf".to_string()));
    }

    #[test]
    fn test_is_command_allowed() {
        let perms = Permissions {
            fs_read: vec!["/".to_string()],
            fs_write: vec!["/".to_string()],
            allowed_commands: vec!["*".to_string()],
            denied_commands: vec!["rm -rf /".to_string()],
        };
        assert!(perms.is_command_allowed("ls -la"));
        assert!(!perms.is_command_allowed("rm -rf /"));
    }

    #[test]
    fn test_is_path_readable() {
        let perms = Permissions {
            fs_read: vec!["/tmp".to_string()],
            fs_write: vec![],
            allowed_commands: vec![],
            denied_commands: vec![],
        };
        assert!(perms.is_path_readable("/tmp/foo.txt"));
        assert!(!perms.is_path_readable("/etc/passwd"));
    }

    #[test]
    fn test_parse_wasm_tools() {
        let toml = r#"
[[tools]]
name = "search_code"
wasm = "plugins/search_code.wasm"
description = "Search code"

[tools.permissions]
mounts = [".:.:ro", "/tmp:/tmp"]
env = ["PATH"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].permissions.mounts, vec![".:.:ro", "/tmp:/tmp"]);
        let resolved = config.tools[0].permissions.resolved_mounts();
        assert_eq!(resolved.len(), 2);
        assert!(!resolved[0].writable);
        assert!(resolved[1].writable);
    }

    #[test]
    fn test_load_missing_file() {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "/nonexistent/xdg/path") };
        let config = load().unwrap();
        assert!(config.model.is_none());
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
}
