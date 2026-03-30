use anyhow::Result;
use std::path::{Path, PathBuf};
use wasmtime::*;
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use crate::config::WasmToolConfig;

pub struct WasmTool {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
    engine: Engine,
    module: Module,
    wasm_config: WasmToolConfig,
}

impl WasmTool {
    pub fn load(cfg: &WasmToolConfig, base_dir: &Path) -> Result<Self> {
        let wasm_path = resolve_wasm_path(&cfg.wasm, base_dir);
        let engine = Engine::default();
        let module = Module::from_file(&engine, &wasm_path)
            .map_err(|e| anyhow::anyhow!("failed to load WASM: {}: {e}", wasm_path.display()))?;

        let manifest = Self::call_manifest(&engine, &module, cfg)?;

        let name = manifest
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&cfg.name)
            .to_string();
        let description = cfg
            .description
            .clone()
            .or_else(|| {
                manifest
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| format!("WASM tool: {name}"));
        let schema = manifest
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));

        Ok(Self {
            name,
            description,
            schema,
            engine,
            module,
            wasm_config: cfg.clone(),
        })
    }

    fn call_manifest(
        engine: &Engine,
        module: &Module,
        cfg: &WasmToolConfig,
    ) -> Result<serde_json::Value> {
        let mut linker = Linker::<WasiP1Ctx>::new(engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |ctx| ctx)?;

        let wasi_ctx = build_wasi_ctx(cfg)?;
        let mut store = Store::new(engine, wasi_ctx);
        let instance = linker.instantiate(&mut store, module)?;

        let manifest_fn = instance
            .get_typed_func::<(), i32>(&mut store, "tool_manifest")
            .map_err(|e| anyhow::anyhow!("WASM must export `tool_manifest() -> i32`: {e}"))?;

        let ptr = manifest_fn.call(&mut store, ())?;
        let json_str = read_wasm_string(&mut store, &instance, ptr)?;
        serde_json::from_str(&json_str)
            .map_err(|e| anyhow::anyhow!("tool_manifest returned invalid JSON: {e}"))
    }

    pub fn execute(&self, input_json: &str) -> Result<String> {
        let mut linker = Linker::<WasiP1Ctx>::new(&self.engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |ctx| ctx)?;

        let wasi_ctx = build_wasi_ctx(&self.wasm_config)?;
        let mut store = Store::new(&self.engine, wasi_ctx);
        let instance = linker.instantiate(&mut store, &self.module)?;

        let alloc_fn = instance
            .get_typed_func::<i32, i32>(&mut store, "alloc_mem")
            .map_err(|e| anyhow::anyhow!("WASM must export `alloc_mem(i32) -> i32`: {e}"))?;

        let input_bytes = input_json.as_bytes();
        let input_ptr = alloc_fn.call(&mut store, input_bytes.len() as i32)?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow::anyhow!("WASM must export `memory`"))?;
        memory.write(&mut store, input_ptr as usize, input_bytes)?;

        let execute_fn = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "execute")
            .map_err(|e| anyhow::anyhow!("WASM must export `execute(i32, i32) -> i32`: {e}"))?;

        let result_ptr = execute_fn.call(&mut store, (input_ptr, input_bytes.len() as i32))?;
        read_wasm_string(&mut store, &instance, result_ptr)
    }
}

fn read_wasm_string(
    store: &mut Store<WasiP1Ctx>,
    instance: &Instance,
    ptr: i32,
) -> Result<String> {
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| anyhow::anyhow!("WASM must export `memory`"))?;
    let data = memory.data(&*store);
    let ptr = ptr as usize;

    anyhow::ensure!(ptr + 8 <= data.len(), "string pointer out of bounds");
    let str_ptr = u32::from_le_bytes(data[ptr..ptr + 4].try_into()?) as usize;
    let str_len = u32::from_le_bytes(data[ptr + 4..ptr + 8].try_into()?) as usize;

    anyhow::ensure!(
        str_ptr + str_len <= data.len(),
        "string data out of bounds"
    );
    String::from_utf8(data[str_ptr..str_ptr + str_len].to_vec())
        .map_err(|e| anyhow::anyhow!("WASM returned invalid UTF-8: {e}"))
}

fn build_wasi_ctx(cfg: &WasmToolConfig) -> Result<WasiP1Ctx> {
    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdio();

    for mount in cfg.permissions.resolved_mounts() {
        let (dir_perms, file_perms) = if mount.writable {
            (DirPerms::all(), FilePerms::all())
        } else {
            (DirPerms::READ, FilePerms::READ)
        };
        builder
            .preopened_dir(&mount.host_path, &mount.guest_path, dir_perms, file_perms)?;
    }

    for key in &cfg.permissions.env {
        if let Ok(val) = std::env::var(key) {
            builder.env(key, &val);
        }
    }

    Ok(builder.build_p1())
}

fn resolve_wasm_path(wasm: &str, base_dir: &Path) -> PathBuf {
    let p = PathBuf::from(wasm);
    if p.is_absolute() {
        p
    } else {
        base_dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_wasm_path_absolute() {
        let result = resolve_wasm_path("/opt/plugins/tool.wasm", Path::new("/home/user/.asobi"));
        assert_eq!(result, PathBuf::from("/opt/plugins/tool.wasm"));
    }

    #[test]
    fn test_resolve_wasm_path_relative() {
        let result = resolve_wasm_path("plugins/tool.wasm", Path::new("/home/user/.asobi"));
        assert_eq!(result, PathBuf::from("/home/user/.asobi/plugins/tool.wasm"));
    }

    fn plugin_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("plugins/{name}.wasm"))
    }

    fn load_plugin(name: &str, mount_dirs: &[&str]) -> Option<WasmTool> {
        let path = plugin_path(name);
        if !path.exists() {
            eprintln!("skipping: {path:?} not found");
            return None;
        }
        let cfg = WasmToolConfig {
            name: name.to_string(),
            wasm: path.to_str().unwrap().to_string(),
            description: None,
            permissions: crate::config::WasmPermissions {
                mounts: mount_dirs.iter().map(|s| s.to_string()).collect(),
                env: vec![],
            },
        };
        Some(WasmTool::load(&cfg, Path::new(".")).unwrap())
    }

    #[test]
    fn test_edit_file_replace() {
        let dir = std::env::temp_dir().join("asobi_wasm_test_edit");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.rs");
        std::fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

        let Some(tool) = load_plugin("edit_file", &[dir.to_str().unwrap()]) else { return };

        let input = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "    println!(\"hello\");",
            "new_string": "    println!(\"world\");"
        });
        let result = tool.execute(&input.to_string()).unwrap();
        assert!(result.contains("replaced 1 occurrence"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("println!(\"world\")"));
        assert!(!content.contains("println!(\"hello\")"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_edit_file_not_found() {
        let dir = std::env::temp_dir().join("asobi_wasm_test_edit_nf");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, "aaa bbb ccc").unwrap();

        let Some(tool) = load_plugin("edit_file", &[dir.to_str().unwrap()]) else { return };

        let input = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "zzz",
            "new_string": "yyy"
        });
        let result = tool.execute(&input.to_string()).unwrap();
        assert!(result.contains("not found"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_edit_file_ambiguous() {
        let dir = std::env::temp_dir().join("asobi_wasm_test_edit_amb");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, "foo bar foo baz foo").unwrap();

        let Some(tool) = load_plugin("edit_file", &[dir.to_str().unwrap()]) else { return };

        let input = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "qux"
        });
        let result = tool.execute(&input.to_string()).unwrap();
        assert!(result.contains("ambiguous"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_patch_file_apply() {
        let dir = std::env::temp_dir().join("asobi_wasm_test_patch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\nline4\n").unwrap();

        let Some(tool) = load_plugin("patch_file", &[dir.to_str().unwrap()]) else { return };

        let patch = "@@ -2,2 +2,2 @@\n-line2\n-line3\n+LINE2\n+LINE3";
        let input = serde_json::json!({
            "path": file.to_str().unwrap(),
            "patch": patch
        });
        let result = tool.execute(&input.to_string()).unwrap();
        eprintln!("patch result: {result}");
        assert!(result.contains("applied 1 hunk"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("LINE2"));
        assert!(content.contains("LINE3"));
        assert!(!content.contains("line2"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_patch_file_context_mismatch() {
        let dir = std::env::temp_dir().join("asobi_wasm_test_patch_mm");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, "aaa\nbbb\nccc\n").unwrap();

        let Some(tool) = load_plugin("patch_file", &[dir.to_str().unwrap()]) else { return };

        let patch = "@@ -1,2 +1,2 @@\n-aaa\n-zzz\n+aaa\n+yyy";
        let input = serde_json::json!({
            "path": file.to_str().unwrap(),
            "patch": patch
        });
        let result = tool.execute(&input.to_string()).unwrap();
        assert!(result.contains("mismatch") || result.contains("Error"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
