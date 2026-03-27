use anyhow::{Context as _, Result};
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ContentBlockDelta, ContentBlockStart, ConversationRole, Message,
    SystemContentBlock, ToolConfiguration, ToolUseBlock,
};
use aws_sdk_bedrockruntime::Client;
use aws_smithy_types::Document;
use tokio::sync::mpsc;

use crate::{history, tools};

const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding assistant. You have access to tools for reading files, writing files, running shell commands, and listing directory contents. Use these tools to help the user with their coding tasks.";

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Text(String),
    ToolCall { name: String, input: String },
    ToolResult { name: String, output: String },
    TurnEnd,
    Error(String),
}

pub struct Agent {
    client: Client,
    model_id: String,
    system_prompt: String,
    session_id: String,
    messages: Vec<Message>,
}

impl Agent {
    pub async fn new(
        model_id: String,
        system_prompt: Option<String>,
        session_id: String,
        restore: bool,
    ) -> Result<Self> {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let client = Client::new(&config);

        let messages = if restore {
            history::load(&session_id).await.unwrap_or_default()
        } else {
            Vec::new()
        };

        let model_id = if model_id.starts_with("arn:") {
            model_id
        } else {
            let region = config
                .region()
                .map(|r| r.as_ref().to_string())
                .unwrap_or_else(|| "us-east-1".to_string());
            format!("arn:aws:bedrock:{region}::foundation-model/{model_id}")
        };

        Ok(Self {
            client,
            model_id,
            system_prompt: system_prompt.unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()),
            session_id,
            messages,
        })
    }

    pub async fn send(&mut self, user_input: &str, tx: mpsc::UnboundedSender<AgentEvent>) {
        let user_msg = Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text(user_input.to_string()))
            .build()
            .context("failed to build user message");

        match user_msg {
            Ok(msg) => self.messages.push(msg),
            Err(e) => {
                let _ = tx.send(AgentEvent::Error(format!("{e:#}")));
                let _ = tx.send(AgentEvent::TurnEnd);
                return;
            }
        }

        if let Err(e) = self.run_loop(tx.clone()).await {
            self.messages.pop();
            let _ = tx.send(AgentEvent::Error(format!("{e:#}")));
        }
        let _ = history::save(&self.session_id, &self.messages).await;
        let _ = tx.send(AgentEvent::TurnEnd);
    }

    async fn run_loop(&mut self, tx: mpsc::UnboundedSender<AgentEvent>) -> Result<()> {
        loop {
            let (text, tool_uses) = self.call_stream(&tx).await?;

            let mut assistant_content: Vec<ContentBlock> = Vec::new();
            if !text.is_empty() {
                assistant_content.push(ContentBlock::Text(text));
            }
            for tu in &tool_uses {
                assistant_content.push(ContentBlock::ToolUse(tu.clone()));
            }

            if assistant_content.is_empty() {
                break;
            }

            let mut builder = Message::builder().role(ConversationRole::Assistant);
            for content in assistant_content {
                builder = builder.content(content);
            }
            self.messages
                .push(builder.build().context("failed to build assistant message")?);

            if tool_uses.is_empty() {
                break;
            }

            let mut tool_result_builder = Message::builder().role(ConversationRole::User);
            for tu in &tool_uses {
                let name = tu.name();
                let tool_use_id = tu.tool_use_id();
                let input = tu.input();
                let input_str = format_document(input);
                let _ = tx.send(AgentEvent::ToolCall {
                    name: name.to_string(),
                    input: input_str,
                });

                let result = tools::execute_tool(name, tool_use_id, input).await;
                let output = result
                    .content()
                    .first()
                    .and_then(|c| match c {
                        aws_sdk_bedrockruntime::types::ToolResultContentBlock::Text(t) => {
                            Some(t.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let _ = tx.send(AgentEvent::ToolResult {
                    name: name.to_string(),
                    output: output.clone(),
                });
                tool_result_builder =
                    tool_result_builder.content(ContentBlock::ToolResult(result));
            }
            self.messages.push(
                tool_result_builder
                    .build()
                    .context("failed to build tool result message")?,
            );
        }
        Ok(())
    }

    async fn call_stream(
        &self,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<(String, Vec<ToolUseBlock>)> {
        let tool_config = ToolConfiguration::builder()
            .set_tools(Some(tools::tool_definitions()))
            .build()
            .context("failed to build tool configuration")?;

        let mut request = self
            .client
            .converse_stream()
            .model_id(&self.model_id)
            .system(SystemContentBlock::Text(self.system_prompt.clone()))
            .tool_config(tool_config);

        for msg in &self.messages {
            request = request.messages(msg.clone());
        }

        let response = request.send().await.context("Bedrock API call failed")?;
        let mut stream = response.stream;

        let mut full_text = String::new();
        let mut tool_uses: Vec<ToolUseBlock> = Vec::new();
        let mut current_tool_name = String::new();
        let mut current_tool_id = String::new();
        let mut current_tool_input = String::new();

        loop {
            match stream.recv().await {
                Ok(Some(event)) => {
                    use aws_sdk_bedrockruntime::types::ConverseStreamOutput;
                    match event {
                        ConverseStreamOutput::ContentBlockStart(start) => {
                            if let Some(ContentBlockStart::ToolUse(tu)) = start.start() {
                                current_tool_name = tu.name().to_string();
                                current_tool_id = tu.tool_use_id().to_string();
                                current_tool_input.clear();
                            }
                        }
                        ConverseStreamOutput::ContentBlockDelta(delta) => {
                            if let Some(d) = delta.delta() {
                                match d {
                                    ContentBlockDelta::Text(t) => {
                                        let _ = tx.send(AgentEvent::Text(t.to_string()));
                                        full_text.push_str(t);
                                    }
                                    ContentBlockDelta::ToolUse(tu) => {
                                        current_tool_input.push_str(tu.input());
                                    }
                                    _ => {}
                                }
                            }
                        }
                        ConverseStreamOutput::ContentBlockStop(_) => {
                            if !current_tool_name.is_empty() {
                                let empty_object =
                                    Document::Object(std::collections::HashMap::new());
                                let input_doc = serde_json::from_str::<serde_json::Value>(
                                    &current_tool_input,
                                )
                                .map(|v| json_value_to_document(&v))
                                .unwrap_or(empty_object);
                                let tool_use = ToolUseBlock::builder()
                                    .tool_use_id(current_tool_id.clone())
                                    .name(current_tool_name.clone())
                                    .input(input_doc)
                                    .build()
                                    .context("failed to build tool use block")?;
                                tool_uses.push(tool_use);
                                current_tool_name.clear();
                                current_tool_id.clear();
                                current_tool_input.clear();
                            }
                        }
                        ConverseStreamOutput::MessageStop(_) => break,
                        _ => {}
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(anyhow::anyhow!("stream error: {e}")),
            }
        }

        Ok((full_text, tool_uses))
    }
}

fn format_document(doc: &Document) -> String {
    let value = document_to_json_value(doc);
    serde_json::to_string(&value).unwrap_or_else(|_| format!("{doc:?}"))
}

fn json_value_to_document(value: &serde_json::Value) -> Document {
    match value {
        serde_json::Value::Null => Document::Null,
        serde_json::Value::Bool(b) => Document::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Document::Number(aws_smithy_types::Number::NegInt(i))
            } else if let Some(f) = n.as_f64() {
                Document::Number(aws_smithy_types::Number::Float(f))
            } else {
                Document::Null
            }
        }
        serde_json::Value::String(s) => Document::String(s.clone()),
        serde_json::Value::Array(arr) => {
            Document::Array(arr.iter().map(json_value_to_document).collect())
        }
        serde_json::Value::Object(obj) => Document::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), json_value_to_document(v)))
                .collect(),
        ),
    }
}

fn document_to_json_value(doc: &Document) -> serde_json::Value {
    match doc {
        Document::Null => serde_json::Value::Null,
        Document::Bool(b) => serde_json::Value::Bool(*b),
        Document::Number(n) => match n {
            aws_smithy_types::Number::PosInt(i) => serde_json::json!(*i),
            aws_smithy_types::Number::NegInt(i) => serde_json::json!(*i),
            aws_smithy_types::Number::Float(f) => serde_json::Value::Number(
                serde_json::Number::from_f64(*f)
                    .unwrap_or_else(|| serde_json::Number::from(0)),
            ),
        },
        Document::String(s) => serde_json::Value::String(s.clone()),
        Document::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(document_to_json_value).collect())
        }
        Document::Object(obj) => {
            let map: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .map(|(k, v)| (k.clone(), document_to_json_value(v)))
                .collect();
            serde_json::Value::Object(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_value_to_document_string() {
        let val = serde_json::json!("hello");
        let doc = json_value_to_document(&val);
        assert!(matches!(doc, Document::String(s) if s == "hello"));
    }

    #[test]
    fn test_json_value_to_document_object() {
        let val = serde_json::json!({"key": "value", "num": 42});
        let doc = json_value_to_document(&val);
        if let Document::Object(map) = &doc {
            assert!(matches!(map.get("key"), Some(Document::String(s)) if s == "value"));
        } else {
            panic!("expected Document::Object");
        }
    }

    #[test]
    fn test_document_roundtrip() {
        let original = serde_json::json!({
            "path": "/tmp/test",
            "content": "hello",
            "nested": {"a": true, "b": [1, 2, 3]}
        });
        let doc = json_value_to_document(&original);
        let back = document_to_json_value(&doc);
        assert_eq!(original, back);
    }

    #[test]
    fn test_format_document() {
        let doc = Document::Object(
            [("path".to_string(), Document::String("/tmp/x".to_string()))]
                .into_iter()
                .collect(),
        );
        let s = format_document(&doc);
        assert!(s.contains("/tmp/x"));
    }
}
