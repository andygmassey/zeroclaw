//! Tool: `whatsapp_read_history`.
//!
//! Reads message history from a specific WhatsApp conversation. Messages are
//! returned in chronological (ascending) order so downstream ingestion into
//! Marvin / PWG can append without reordering.
//!
//! # Inputs
//!
//! - `chat_id` (string, required) – JID (e.g. `447000000000@s.whatsapp.net`,
//!   `1234567890@g.us`) or a contact-name substring. Names are matched
//!   case-insensitively against the chat's stored display name.
//! - `limit` (integer, optional, default `50`, capped at `500`) – maximum
//!   messages to return.
//! - `since` (string, optional) – ISO-8601 timestamp. When present, returns
//!   only messages with `timestamp > since`.
//! - `since_unix` (integer, optional) – Unix timestamp in seconds. Preferred
//!   over `since` when already available as epoch; if both are supplied the
//!   larger of the two wins.
//!
//! # Output
//!
//! ```json
//! {
//!   "chat_id": "1234567890@s.whatsapp.net",
//!   "chat_name": "Ada Lovelace",
//!   "messages": [
//!     {
//!       "id": "…",
//!       "from": "Ada",
//!       "from_me": false,
//!       "body": "Hello",
//!       "timestamp": "2026-04-24T08:12:01+00:00",
//!       "timestamp_unix": 1714032721,
//!       "attachment": { "present": false, "type": "text" }
//!     }
//!   ],
//!   "returned": 1,
//!   "limit": 50,
//!   "has_more": false,
//!   "next_since_unix": 1714032721
//! }
//! ```
//!
//! Attachment bodies are never downloaded by this tool – only metadata. Media
//! bytes would require separate, policy-gated fetches.

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
        "Read recent messages from a WhatsApp conversation in chronological order. \
         Provide the chat_id (JID) from whatsapp_list_chats, or a contact name \
         (fuzzy-matched against stored chat names). Supports incremental reads \
         via `since` (ISO-8601) or `since_unix` (epoch seconds). Attachment \
         metadata is included; media bodies are never downloaded."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "Chat JID from whatsapp_list_chats, or a contact name to fuzzy-match"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum messages to return (default 50, max 500)",
                    "default": 50,
                    "minimum": 1,
                    "maximum": 500
                },
                "since": {
                    "type": "string",
                    "description": "Only messages after this ISO-8601 timestamp (for incremental reads)"
                },
                "since_unix": {
                    "type": "integer",
                    "description": "Only messages after this Unix timestamp in seconds (alternative to `since`)"
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
            let _ = args;
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
                .unwrap_or(50)
                .min(500) as usize;

            // Prefer `since_unix` if both are provided, otherwise parse `since`.
            let since_from_iso = args
                .get("since")
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp())
                });
            let since_from_unix = args.get("since_unix").and_then(|v| v.as_i64());
            let since_ts = match (since_from_iso, since_from_unix) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };

            // Resolve chat_id: if it doesn't look like a JID, try fuzzy name match.
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
                    let returned = messages.len();
                    let newest_ts = messages.last().map(|m| m.timestamp);
                    let msg_list: Vec<serde_json::Value> = messages
                        .into_iter()
                        .map(|m| {
                            let attachment = json!({
                                "present": m.has_media,
                                "type": m.msg_type,
                            });
                            json!({
                                "id": m.id,
                                "from": m.sender_name,
                                "from_me": m.from_me,
                                "body": m.body,
                                "timestamp": chrono::DateTime::from_timestamp(m.timestamp, 0)
                                    .map(|dt| dt.to_rfc3339())
                                    .unwrap_or_else(|| m.timestamp.to_string()),
                                "timestamp_unix": m.timestamp,
                                "attachment": attachment,
                            })
                        })
                        .collect();

                    let mut result = json!({
                        "chat_id": chat_id,
                        "chat_name": chat_name,
                        "messages": msg_list,
                        "returned": returned,
                        "limit": limit,
                        // A full page usually means more is available; not
                        // authoritative, but good enough as a cursor hint.
                        "has_more": returned >= limit,
                    });
                    if let Some(ts) = newest_ts {
                        result["next_since_unix"] = json!(ts);
                    }

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
        // Both since variants must be advertised.
        assert!(schema["properties"]["since"].is_object());
        assert!(schema["properties"]["since_unix"].is_object());
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn execute_returns_chronological_messages_with_cursor() {
        use crate::channels::whatsapp_message_store::{StoredMessage, WhatsAppMessageStore};

        let tmp = tempfile::TempDir::new().unwrap();
        let jid = "1234567890@s.whatsapp.net";
        {
            let store =
                WhatsAppMessageStore::new(&tmp.path().join("whatsapp-messages.db")).unwrap();
            for i in 0..6 {
                store
                    .upsert_message(&StoredMessage {
                        id: format!("msg_{i}"),
                        chat_id: jid.into(),
                        chat_name: "Test Contact".into(),
                        sender_name: if i % 2 == 0 { "Test Contact" } else { "Andy" }.into(),
                        from_me: i % 2 != 0,
                        body: format!("msg {i}"),
                        timestamp: 1_700_000_000 + i,
                        has_media: i == 5,
                        msg_type: if i == 5 { "image".into() } else { "chat".into() },
                    })
                    .unwrap();
            }
        }

        let tool = WhatsAppReadHistoryTool::new(
            Arc::new(SecurityPolicy::default()),
            tmp.path(),
        );
        let result = tool
            .execute(json!({ "chat_id": jid, "limit": 10 }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["returned"], 6);
        assert_eq!(parsed["chat_id"], jid);
        // Chronological order preserved.
        let msgs = parsed["messages"].as_array().unwrap();
        let ts: Vec<i64> = msgs
            .iter()
            .map(|m| m["timestamp_unix"].as_i64().unwrap())
            .collect();
        assert!(ts.windows(2).all(|w| w[0] <= w[1]));
        // Attachment metadata on the final image-type message.
        assert_eq!(msgs[5]["attachment"]["present"], true);
        assert_eq!(msgs[5]["attachment"]["type"], "image");
        // Cursor for incremental reads.
        assert_eq!(parsed["next_since_unix"], 1_700_000_005_i64);
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn execute_incremental_read_via_since_unix() {
        use crate::channels::whatsapp_message_store::{StoredMessage, WhatsAppMessageStore};

        let tmp = tempfile::TempDir::new().unwrap();
        let jid = "1234567890@s.whatsapp.net";
        {
            let store =
                WhatsAppMessageStore::new(&tmp.path().join("whatsapp-messages.db")).unwrap();
            for i in 0..6 {
                store
                    .upsert_message(&StoredMessage {
                        id: format!("msg_{i}"),
                        chat_id: jid.into(),
                        chat_name: "Test Contact".into(),
                        sender_name: "Test Contact".into(),
                        from_me: false,
                        body: format!("msg {i}"),
                        timestamp: 1_700_000_000 + i,
                        has_media: false,
                        msg_type: "chat".into(),
                    })
                    .unwrap();
            }
        }

        let tool = WhatsAppReadHistoryTool::new(
            Arc::new(SecurityPolicy::default()),
            tmp.path(),
        );
        let result = tool
            .execute(json!({
                "chat_id": jid,
                "since_unix": 1_700_000_002_i64
            }))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        // Should return msg_3, msg_4, msg_5 (timestamps 3, 4, 5; strictly > 2).
        assert_eq!(parsed["returned"], 3);
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn execute_errors_clearly_when_chat_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WhatsAppReadHistoryTool::new(
            Arc::new(SecurityPolicy::default()),
            tmp.path(),
        );
        // Not a JID, not in an empty store.
        let result = tool
            .execute(json!({ "chat_id": "Nonexistent Person" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("No chat found"));
    }
}
