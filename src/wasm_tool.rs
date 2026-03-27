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

    for path in &cfg.permissions.fs_read {
        builder
            .preopened_dir(path, path, DirPerms::READ, FilePerms::READ)?;
    }

    for path in &cfg.permissions.fs_write {
        builder
            .preopened_dir(path, path, DirPerms::all(), FilePerms::all())?;
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

    fn wasm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/sandboxed_exec.wasm")
    }

    #[test]
    fn test_load_sandboxed_exec_plugin() {
        let path = wasm_path();
        if !path.exists() {
            eprintln!("skipping: {path:?} not found");
            return;
        }
        let cfg = WasmToolConfig {
            name: "sandboxed_exec".to_string(),
            wasm: path.to_str().unwrap().to_string(),
            description: None,
            permissions: Default::default(),
        };
        let tool = WasmTool::load(&cfg, Path::new(".")).unwrap();
        assert_eq!(tool.name, "sandboxed_exec");
        assert!(tool.description.contains("sandbox"));
        assert!(tool.schema.get("properties").is_some());
    }

    #[test]
    fn test_execute_sandboxed_exec_ls() {
        let path = wasm_path();
        if !path.exists() {
            eprintln!("skipping: {path:?} not found");
            return;
        }

        let test_dir = std::env::temp_dir().join("asobi_wasm_test_exec");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("hello.txt"), "hello wasm").unwrap();

        let cfg = WasmToolConfig {
            name: "sandboxed_exec".to_string(),
            wasm: path.to_str().unwrap().to_string(),
            description: None,
            permissions: crate::config::WasmPermissions {
                fs_read: vec![test_dir.to_str().unwrap().to_string()],
                fs_write: vec![],
                env: vec![],
            },
        };
        let tool = WasmTool::load(&cfg, Path::new(".")).unwrap();

        let input = serde_json::json!({
            "code": format!("ls {}\nread {}/hello.txt", test_dir.display(), test_dir.display())
        });
        let result = tool.execute(&input.to_string()).unwrap();
        eprintln!("output: {result}");
        assert!(result.contains("hello.txt"));
        assert!(result.contains("hello wasm"));

        let _ = std::fs::remove_dir_all(&test_dir);
    }
}
