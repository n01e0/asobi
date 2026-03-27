use anyhow::{Context as _, Result};
use aws_sdk_bedrockruntime::types::{ContentBlock, ConversationRole, Message};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::AsyncBufReadExt;

#[derive(Serialize, Deserialize)]
struct HistoryEntry {
    role: String,
    content: Vec<ContentEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum ContentEntry {
    #[serde(rename = "text")]
    Text { text: String },
}

fn sessions_dir() -> PathBuf {
    std::env::var("ASOBI_HISTORY_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("XDG_CONFIG_HOME")
                .ok()
                .map(|xdg| PathBuf::from(xdg).join("asobi"))
        })
        .or_else(|| home::home_dir().map(|h| h.join(".asobi")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sessions")
}

fn session_path(session_id: &str) -> PathBuf {
    sessions_dir().join(format!("{session_id}.jsonl"))
}

pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn latest_session_id() -> Result<Option<String>> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Ok(None);
    }

    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl")
            && let Ok(meta) = entry.metadata()
        {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            let id = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
                newest = Some((mtime, id));
            }
        }
    }
    Ok(newest.map(|(_, id)| id))
}

pub async fn load(session_id: &str) -> Result<Vec<Message>> {
    let path = session_path(session_id);
    if !path.exists() {
        anyhow::bail!("session not found: {session_id}");
    }

    let file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    let reader = tokio::io::BufReader::new(file);
    let mut lines = reader.lines();
    let mut messages = Vec::new();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line)
            && let Some(msg) = entry_to_message(&entry)
        {
            messages.push(msg);
        }
    }
    Ok(messages)
}

pub async fn save(session_id: &str, messages: &[Message]) -> Result<()> {
    let path = session_path(session_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut content = String::new();
    for msg in messages {
        if let Some(entry) = message_to_entry(msg) {
            content.push_str(&serde_json::to_string(&entry)?);
            content.push('\n');
        }
    }

    tokio::fs::write(&path, content.as_bytes())
        .await
        .with_context(|| format!("failed to write to {}", path.display()))?;

    Ok(())
}

fn message_to_entry(msg: &Message) -> Option<HistoryEntry> {
    let role = match msg.role() {
        ConversationRole::User => "user",
        ConversationRole::Assistant => "assistant",
        _ => return None,
    };

    let content: Vec<ContentEntry> = msg
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(ContentEntry::Text { text: text.clone() }),
            _ => None,
        })
        .collect();

    if content.is_empty() {
        return None;
    }

    Some(HistoryEntry {
        role: role.to_string(),
        content,
    })
}

fn entry_to_message(entry: &HistoryEntry) -> Option<Message> {
    let role = match entry.role.as_str() {
        "user" => ConversationRole::User,
        "assistant" => ConversationRole::Assistant,
        _ => return None,
    };

    let mut builder = Message::builder().role(role);
    for c in &entry.content {
        match c {
            ContentEntry::Text { text } => {
                builder = builder.content(ContentBlock::Text(text.clone()));
            }
        }
    }
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_session_id_is_uuid() {
        let id = new_session_id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_message_to_entry_user() {
        let msg = Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text("hello".to_string()))
            .build()
            .unwrap();
        let entry = message_to_entry(&msg).unwrap();
        assert_eq!(entry.role, "user");
        assert_eq!(entry.content.len(), 1);
    }

    #[test]
    fn test_message_to_entry_assistant() {
        let msg = Message::builder()
            .role(ConversationRole::Assistant)
            .content(ContentBlock::Text("response".to_string()))
            .build()
            .unwrap();
        let entry = message_to_entry(&msg).unwrap();
        assert_eq!(entry.role, "assistant");
    }

    #[test]
    fn test_entry_to_message_roundtrip() {
        let msg = Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text("test".to_string()))
            .build()
            .unwrap();
        let entry = message_to_entry(&msg).unwrap();
        let restored = entry_to_message(&entry).unwrap();
        assert_eq!(*restored.role(), ConversationRole::User);
        assert!(matches!(&restored.content()[0], ContentBlock::Text(t) if t == "test"));
    }

    #[test]
    fn test_entry_serialization_roundtrip() {
        let entry = HistoryEntry {
            role: "user".to_string(),
            content: vec![ContentEntry::Text {
                text: "hello world".to_string(),
            }],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, "user");
        assert_eq!(deserialized.content.len(), 1);
    }

    #[test]
    fn test_entry_to_message_unknown_role() {
        let entry = HistoryEntry {
            role: "system".to_string(),
            content: vec![ContentEntry::Text {
                text: "x".to_string(),
            }],
        };
        assert!(entry_to_message(&entry).is_none());
    }

    #[tokio::test]
    async fn test_save_load_and_nonexistent() {
        let dir = std::env::temp_dir().join("asobi_test_sessions_all");
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("ASOBI_HISTORY_DIR", &dir) };

        let result = load("nonexistent-id").await;
        assert!(result.is_err());

        let session_id = new_session_id();
        let messages = vec![
            Message::builder()
                .role(ConversationRole::User)
                .content(ContentBlock::Text("question".to_string()))
                .build()
                .unwrap(),
            Message::builder()
                .role(ConversationRole::Assistant)
                .content(ContentBlock::Text("answer".to_string()))
                .build()
                .unwrap(),
        ];

        save(&session_id, &messages).await.unwrap();
        let loaded = load(&session_id).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(*loaded[0].role(), ConversationRole::User);
        assert_eq!(*loaded[1].role(), ConversationRole::Assistant);

        let latest = latest_session_id().unwrap();
        assert_eq!(latest, Some(session_id));

        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("ASOBI_HISTORY_DIR") };
    }
}
