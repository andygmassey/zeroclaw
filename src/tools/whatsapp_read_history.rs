//! Tool: whatsapp_read_history
//!
//! Reads message history from a specific WhatsApp conversation.
//! Supports incremental reads via the `since` timestamp parameter.

use super::traits::{Tool, ToolResult};
use crate::channels::whatsapp_message_store::WhatsAppMessageStore;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Agent-callable tool for reading WhatsApp conversation history.
pub struct WhatsAppReadHistoryTool {
    store_path: PathBuf,
    security: Arc<SecurityPolicy>,
}

impl WhatsAppReadHistoryTool {
    pub fn new(security: Arc<SecurityPolicy>, zeroclaw_dir: &std::path::Path) -> Self {
        Self {
            store_path: zeroclaw_dir.join("whatsapp-messages.db"),
            security,
        }
    }

    #[cfg(feature = "whatsapp-web")]
    fn open_store(&self) -> anyhow::Result<WhatsAppMessageStore> {
        WhatsAppMessageStore::new(&self.store_path)
    }
}

#[async_trait]
impl Tool for WhatsAppReadHistoryTool {
    fn name(&self) -> &str {
        "whatsapp_read_history"
    }

    fn description(&self) -> &str {
        "Read recent messages from a WhatsApp conversation. \
         Provide the chat_id from whatsapp_list_chats, or a contact name \
         (will fuzzy-match). Returns messages in chronological order."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "Chat ID from whatsapp_list_chats, or a contact name to search for"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum messages to return (default 50)",
                    "default": 50
                },
                "since": {
                    "type": "string",
                    "description": "Only messages after this ISO timestamp (for incremental reads)"
                }
            },
            "required": ["chat_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "whatsapp_read_history")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        #[cfg(not(feature = "whatsapp-web"))]
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("WhatsApp tools require the 'whatsapp-web' feature".to_string()),
            });
        }

        #[cfg(feature = "whatsapp-web")]
        {
            let store = match self.open_store() {
                Ok(s) => s,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Failed to open WhatsApp message store: {e}"
                        )),
                    });
                }
            };

            let raw_chat_id = args
                .get("chat_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'chat_id' parameter"))?;

            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;

            let since_ts = args
                .get("since")
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp())
                });

            // Resolve chat_id: if it doesn't look like a JID, try fuzzy name match
            let chat_id = if raw_chat_id.contains('@') {
                raw_chat_id.to_string()
            } else {
                match store.find_chat_by_name(raw_chat_id) {
                    Ok(Some(id)) => id,
                    Ok(None) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!(
                                "No chat found matching '{raw_chat_id}'. \
                                 Use whatsapp_list_chats to see available conversations."
                            )),
                        });
                    }
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Failed to search for chat: {e}")),
                        });
                    }
                }
            };

            match store.read_history(&chat_id, limit, since_ts) {
                Ok((messages, chat_name)) => {
                    let total = messages.len();
                    let msg_list: Vec<serde_json::Value> = messages
                        .into_iter()
                        .map(|m| {
                            json!({
                                "id": m.id,
                                "from": m.sender_name,
                                "from_me": m.from_me,
                                "body": m.body,
                                "timestamp": chrono::DateTime::from_timestamp(m.timestamp, 0)
                                    .map(|dt| dt.to_rfc3339())
                                    .unwrap_or_else(|| m.timestamp.to_string()),
                                "has_media": m.has_media,
                                "type": m.msg_type,
                            })
                        })
                        .collect();

                    let result = json!({
                        "chat_id": chat_id,
                        "chat_name": chat_name,
                        "messages": msg_list,
                        "returned": total,
                        "has_more": total >= limit,
                    });

                    Ok(ToolResult {
                        success: true,
                        output: serde_json::to_string_pretty(&result).unwrap_or_default(),
                        error: None,
                    })
                }
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read history: {e}")),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_metadata() {
        let tool = WhatsAppReadHistoryTool::new(
            Arc::new(SecurityPolicy::default()),
            std::path::Path::new("/tmp"),
        );
        assert_eq!(tool.name(), "whatsapp_read_history");
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "chat_id"));
    }
}
