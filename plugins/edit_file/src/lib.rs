use std::alloc::{alloc, Layout};

#[repr(C)]
pub struct WasmString {
    ptr: u32,
    len: u32,
}

fn make_wasm_string(s: &str) -> *const WasmString {
    let bytes = s.as_bytes();
    let data_layout = Layout::from_size_align(bytes.len().max(1), 1).unwrap();
    let data_ptr = unsafe { alloc(data_layout) };
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr, bytes.len()) };

    let header_layout = Layout::new::<WasmString>();
    let header_ptr = unsafe { alloc(header_layout) as *mut WasmString };
    unsafe {
        (*header_ptr).ptr = data_ptr as u32;
        (*header_ptr).len = bytes.len() as u32;
    }
    header_ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn tool_manifest() -> *const WasmString {
    let manifest = serde_json::json!({
        "name": "edit_file",
        "description": "Edit a file by replacing a unique string with a new string. If old_string is empty, inserts new_string at the beginning of the file (or creates the file).",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace. Must match uniquely. Empty string means insert at beginning."
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string"
                }
            },
            "required": ["path", "old_string", "new_string"]
        }
    });
    make_wasm_string(&manifest.to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn alloc_mem(size: i32) -> *mut u8 {
    let layout = Layout::from_size_align(size as usize, 1).unwrap();
    unsafe { alloc(layout) }
}

#[unsafe(no_mangle)]
pub extern "C" fn execute(ptr: i32, len: i32) -> *const WasmString {
    let input_bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let input_str = std::str::from_utf8(input_bytes).unwrap_or("{}");

    let result = match run(input_str) {
        Ok(output) => output,
        Err(e) => format!("Error: {e}"),
    };
    make_wasm_string(&result)
}

fn run(input_str: &str) -> Result<String, String> {
    let input: serde_json::Value =
        serde_json::from_str(input_str).map_err(|e| format!("invalid JSON: {e}"))?;

    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing 'path'")?;
    let old_string = input
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or("missing 'old_string'")?;
    let new_string = input
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or("missing 'new_string'")?;

    if old_string.is_empty() {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        let content = format!("{new_string}{existing}");
        write_file(path, &content)?;
        return Ok(format!("Inserted at beginning of {path}"));
    }

    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;

    let count = content.matches(old_string).count();
    match count {
        0 => Err(format!(
            "old_string not found in {path}. Make sure it matches exactly (including whitespace and indentation)."
        )),
        1 => {
            let new_content = content.replacen(old_string, new_string, 1);
            write_file(path, &new_content)?;
            Ok(format!("Edited {path}: replaced 1 occurrence"))
        }
        n => Err(format!(
            "old_string is ambiguous: found {n} occurrences in {path}. Include more surrounding context to make it unique."
        )),
    }
}

fn write_file(path: &str, content: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create dir: {e}"))?;
        }
    }
    std::fs::write(path, content).map_err(|e| format!("failed to write {path}: {e}"))
}
