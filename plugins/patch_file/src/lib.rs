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
        "name": "patch_file",
        "description": "Apply a unified diff patch to a file. The patch should be in standard unified diff format (output of `diff -u` or `git diff`).",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to patch"
                },
                "patch": {
                    "type": "string",
                    "description": "Unified diff patch content. Lines starting with '-' are removed, '+' are added, ' ' (space) are context."
                }
            },
            "required": ["path", "patch"]
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
    let patch = input
        .get("patch")
        .and_then(|v| v.as_str())
        .ok_or("missing 'patch'")?;

    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;

    let hunks = parse_hunks(patch)?;
    let new_content = apply_hunks(&content, &hunks)?;

    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create dir: {e}"))?;
        }
    }
    std::fs::write(path, &new_content).map_err(|e| format!("failed to write {path}: {e}"))?;

    Ok(format!(
        "Patched {path}: applied {} hunk(s)",
        hunks.len()
    ))
}

struct Hunk {
    old_start: usize,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

fn parse_hunks(patch: &str) -> Result<Vec<Hunk>, String> {
    let mut hunks = Vec::new();
    let mut lines = patch.lines().peekable();

    while let Some(line) = lines.peek() {
        if line.starts_with("---") || line.starts_with("+++") || line.starts_with("diff ") {
            lines.next();
            continue;
        }
        if line.starts_with("@@") {
            let hunk_header = lines.next().unwrap();
            let (old_start, _old_count) = parse_hunk_header(hunk_header)?;

            let mut old_lines = Vec::new();
            let mut new_lines = Vec::new();

            while let Some(&next) = lines.peek() {
                if next.starts_with("@@") || next.starts_with("diff ") {
                    break;
                }
                let next = lines.next().unwrap();
                if let Some(stripped) = next.strip_prefix('-') {
                    old_lines.push(stripped.to_string());
                } else if let Some(stripped) = next.strip_prefix('+') {
                    new_lines.push(stripped.to_string());
                } else if let Some(stripped) = next.strip_prefix(' ') {
                    old_lines.push(stripped.to_string());
                    new_lines.push(stripped.to_string());
                } else {
                    old_lines.push(next.to_string());
                    new_lines.push(next.to_string());
                }
            }

            hunks.push(Hunk {
                old_start,
                old_lines,
                new_lines,
            });
        } else {
            lines.next();
        }
    }

    if hunks.is_empty() {
        return Err("no hunks found in patch".to_string());
    }
    Ok(hunks)
}

fn parse_hunk_header(header: &str) -> Result<(usize, usize), String> {
    // @@ -OLD_START,OLD_COUNT +NEW_START,NEW_COUNT @@
    let header = header.trim_start_matches("@@").trim();
    let parts: Vec<&str> = header.split("@@").next().unwrap_or("").trim().split(' ').collect();

    let old_part = parts
        .first()
        .ok_or("invalid hunk header")?
        .trim_start_matches('-');
    let (start, count) = if let Some((s, c)) = old_part.split_once(',') {
        (
            s.parse::<usize>().map_err(|e| format!("bad line number: {e}"))?,
            c.parse::<usize>().map_err(|e| format!("bad count: {e}"))?,
        )
    } else {
        (
            old_part.parse::<usize>().map_err(|e| format!("bad line number: {e}"))?,
            1,
        )
    };

    Ok((start, count))
}

fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut result: Vec<String> = Vec::new();
    let mut pos: usize = 0;

    for hunk in hunks {
        let start = if hunk.old_start == 0 { 0 } else { hunk.old_start - 1 };

        if start < pos {
            return Err(format!(
                "overlapping hunks at line {}",
                hunk.old_start
            ));
        }

        for line in &lines[pos..start] {
            result.push(line.to_string());
        }

        let end = start + hunk.old_lines.len();
        if end > lines.len() {
            return Err(format!(
                "hunk extends beyond file (line {} + {} lines, file has {} lines)",
                hunk.old_start,
                hunk.old_lines.len(),
                lines.len()
            ));
        }

        for (i, expected) in hunk.old_lines.iter().enumerate() {
            let actual = lines[start + i];
            if actual != expected {
                return Err(format!(
                    "context mismatch at line {}: expected {:?}, got {:?}",
                    start + i + 1,
                    expected,
                    actual
                ));
            }
        }

        for line in &hunk.new_lines {
            result.push(line.clone());
        }

        pos = end;
    }

    for line in &lines[pos..] {
        result.push(line.to_string());
    }

    let mut output = result.join("\n");
    if content.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}
