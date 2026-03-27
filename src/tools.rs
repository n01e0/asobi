use anyhow::{Context as _, Result};
use aws_sdk_bedrockruntime::types::{
    Tool, ToolInputSchema, ToolResultBlock, ToolResultContentBlock, ToolSpecification,
};
use aws_smithy_types::Document;
use std::collections::HashMap;
use std::path::Path;

pub fn tool_definitions() -> Vec<Tool> {
    vec![
        read_file_def(),
        write_file_def(),
        run_command_def(),
        list_files_def(),
    ]
}

fn json_schema(properties: Document, required: Vec<&str>) -> Document {
    Document::Object(HashMap::from([
        ("type".into(), Document::String("object".into())),
        ("properties".into(), properties),
        (
            "required".into(),
            Document::Array(required.into_iter().map(|s| Document::String(s.into())).collect()),
        ),
    ]))
}

fn string_prop(description: &str) -> Document {
    Document::Object(HashMap::from([
        ("type".into(), Document::String("string".into())),
        ("description".into(), Document::String(description.into())),
    ]))
}

fn read_file_def() -> Tool {
    let schema = json_schema(
        Document::Object(HashMap::from([(
            "path".into(),
            string_prop("Path to the file to read"),
        )])),
        vec!["path"],
    );
    Tool::ToolSpec(
        ToolSpecification::builder()
            .name("read_file")
            .description("Read the contents of a file")
            .input_schema(ToolInputSchema::Json(schema))
            .build()
            .expect("static tool definition"),
    )
}

fn write_file_def() -> Tool {
    let schema = json_schema(
        Document::Object(HashMap::from([
            ("path".into(), string_prop("Path to the file to write")),
            ("content".into(), string_prop("Content to write to the file")),
        ])),
        vec!["path", "content"],
    );
    Tool::ToolSpec(
        ToolSpecification::builder()
            .name("write_file")
            .description("Write content to a file, creating it if it doesn't exist")
            .input_schema(ToolInputSchema::Json(schema))
            .build()
            .expect("static tool definition"),
    )
}

fn run_command_def() -> Tool {
    let schema = json_schema(
        Document::Object(HashMap::from([(
            "command".into(),
            string_prop("Shell command to execute"),
        )])),
        vec!["command"],
    );
    Tool::ToolSpec(
        ToolSpecification::builder()
            .name("run_command")
            .description("Execute a shell command and return its stdout and stderr")
            .input_schema(ToolInputSchema::Json(schema))
            .build()
            .expect("static tool definition"),
    )
}

fn list_files_def() -> Tool {
    let schema = json_schema(
        Document::Object(HashMap::from([(
            "path".into(),
            string_prop("Directory path to list files in"),
        )])),
        vec!["path"],
    );
    Tool::ToolSpec(
        ToolSpecification::builder()
            .name("list_files")
            .description("List files and directories in the specified path")
            .input_schema(ToolInputSchema::Json(schema))
            .build()
            .expect("static tool definition"),
    )
}

fn get_string_param<'a>(input: &'a Document, key: &str) -> Result<&'a str> {
    match input {
        Document::Object(map) => match map.get(key) {
            Some(Document::String(s)) => Ok(s.as_str()),
            _ => anyhow::bail!("missing or invalid parameter: {key}"),
        },
        _ => anyhow::bail!("input is not an object"),
    }
}

pub async fn execute_tool(name: &str, tool_use_id: &str, input: &Document) -> ToolResultBlock {
    let result = match name {
        "read_file" => exec_read_file(input).await,
        "write_file" => exec_write_file(input).await,
        "run_command" => exec_run_command(input).await,
        "list_files" => exec_list_files(input).await,
        _ => Err(anyhow::anyhow!("unknown tool: {name}")),
    };

    let (text, status) = match result {
        Ok(output) => (output, None),
        Err(e) => (
            format!("Error: {e:#}"),
            Some(aws_sdk_bedrockruntime::types::ToolResultStatus::Error),
        ),
    };

    let mut builder = ToolResultBlock::builder()
        .tool_use_id(tool_use_id)
        .content(ToolResultContentBlock::Text(text.clone()));
    if let Some(s) = status {
        builder = builder.status(s);
    }

    match builder.build() {
        Ok(block) => block,
        Err(e) => {
            // tool_use_id + content が揃っている以上ここには到達しないが、
            // 万一の場合はエラー内容をテキストとして返す
            ToolResultBlock::builder()
                .tool_use_id(tool_use_id)
                .content(ToolResultContentBlock::Text(format!(
                    "Internal error building tool result: {e}"
                )))
                .status(aws_sdk_bedrockruntime::types::ToolResultStatus::Error)
                .build()
                .expect("fallback tool result with all required fields")
        }
    }
}

async fn exec_read_file(input: &Document) -> Result<String> {
    let path = get_string_param(input, "path")?;
    tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read {path}"))
}

async fn exec_write_file(input: &Document) -> Result<String> {
    let path = get_string_param(input, "path")?;
    let content = get_string_param(input, "content")?;
    if let Some(parent) = Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, content)
        .await
        .with_context(|| format!("failed to write {path}"))?;
    Ok(format!("Written to {path}"))
}

async fn exec_run_command(input: &Document) -> Result<String> {
    let command = get_string_param(input, "command")?;
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
        .with_context(|| format!("failed to execute: {command}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);
    Ok(format!(
        "exit code: {exit_code}\n--- stdout ---\n{stdout}--- stderr ---\n{stderr}"
    ))
}

async fn exec_list_files(input: &Document) -> Result<String> {
    let path = get_string_param(input, "path")?;
    let mut entries = tokio::fs::read_dir(path)
        .await
        .with_context(|| format!("failed to read directory {path}"))?;

    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        let suffix = if file_type.is_dir() { "/" } else { "" };
        names.push(format!("{}{suffix}", entry.file_name().to_string_lossy()));
    }
    names.sort();
    Ok(names.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(pairs: &[(&str, &str)]) -> Document {
        Document::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Document::String(v.to_string())))
                .collect(),
        )
    }

    #[test]
    fn test_tool_definitions_count() {
        assert_eq!(tool_definitions().len(), 4);
    }

    #[test]
    fn test_get_string_param_ok() {
        let input = make_input(&[("path", "/tmp/test")]);
        assert_eq!(get_string_param(&input, "path").unwrap(), "/tmp/test");
    }

    #[test]
    fn test_get_string_param_missing() {
        let input = make_input(&[("path", "/tmp/test")]);
        assert!(get_string_param(&input, "missing").is_err());
    }

    #[test]
    fn test_get_string_param_not_object() {
        let input = Document::String("hello".into());
        assert!(get_string_param(&input, "path").is_err());
    }

    #[tokio::test]
    async fn test_read_file() {
        let dir = std::env::temp_dir().join("asobi_test_read");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let input = make_input(&[("path", file.to_str().unwrap())]);
        let result = exec_read_file(&input).await.unwrap();
        assert_eq!(result, "hello world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_write_file() {
        let dir = std::env::temp_dir().join("asobi_test_write");
        let file = dir.join("out.txt");

        let input = make_input(&[("path", file.to_str().unwrap()), ("content", "test data")]);
        let result = exec_write_file(&input).await.unwrap();
        assert!(result.contains("Written to"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "test data");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_run_command() {
        let input = make_input(&[("command", "echo hello")]);
        let result = exec_run_command(&input).await.unwrap();
        assert!(result.contains("exit code: 0"));
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_list_files() {
        let dir = std::env::temp_dir().join("asobi_test_list");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();

        let input = make_input(&[("path", dir.to_str().unwrap())]);
        let result = exec_list_files(&input).await.unwrap();
        assert!(result.contains("a.txt"));
        assert!(result.contains("b.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        let input = Document::Object(HashMap::new());
        let result = execute_tool("nonexistent", "id-1", &input).await;
        assert_eq!(
            result.status(),
            Some(&aws_sdk_bedrockruntime::types::ToolResultStatus::Error)
        );
    }
}
