//! Tool: `whatsapp_list_chats`.
//!
//! Lists WhatsApp conversations visible to the connected WhatsApp Web session,
//! sorted by most recent activity. Results are drawn from the shared message
//! store at `~/.zeroclaw/whatsapp-messages.db`, populated by the
//! [`WhatsAppWebChannel`](crate::channels::whatsapp_web::WhatsAppWebChannel).
//!
//! # Inputs
//!
//! - `limit` (integer, optional, default `20`) – maximum chats to return.
//! - `offset` (integer, optional, default `0`) – pagination offset.
//! - `include_groups` (boolean, optional, default `false`) – include group
//!   chats alongside 1:1 conversations.
//!
//! # Output
//!
//! ```json
//! {
//!   "chats": [
//!     {
//!       "id": "1234567890@s.whatsapp.net",
//!       "name": "Ada Lovelace",
//!       "chat_type": "individual",
//!       "is_group": false,
//!       "last_message_time": "2026-04-24T08:12:01+00:00",
//!       "message_count": 42,
//!       "unread_count": 0
//!     }
//!   ],
//!   "returned": 1,
//!   "total": 12,
//!   "offset": 0,
//!   "limit": 20,
//!   "has_more": false
//! }
//! ```

use super::traits::{Tool, ToolResult};
use crate::channels::whatsapp_message_store::WhatsAppMessageStore;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Agent-callable tool for listing WhatsApp conversations.
pub struct WhatsAppListChatsTool {
    store_path: PathBuf,
    security: Arc<SecurityPolicy>,
}

impl WhatsAppListChatsTool {
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
impl Tool for WhatsAppListChatsTool {
    fn name(&self) -> &str {
        "whatsapp_list_chats"
    }

    fn description(&self) -> &str {
        "List WhatsApp conversations sorted by most recent message. \
         Returns chat id (JID), name, chat_type (individual or group), \
         last message time, and message count. \
         By default only shows 1:1 chats; set include_groups=true for groups. \
         Supports limit + offset pagination."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum chats to return (default 20, capped at 500)",
                    "default": 20,
                    "minimum": 1,
                    "maximum": 500
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of chats to skip for pagination (default 0)",
                    "default": 0,
                    "minimum": 0
                },
                "include_groups": {
                    "type": "boolean",
                    "description": "Include group chats (default false)",
                    "default": false
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "whatsapp_list_chats")
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
                            "Failed to open WhatsApp message store at {}: {e}. \
                             Messages are stored once the WhatsApp Web channel connects.",
                            self.store_path.display()
                        )),
                    });
                }
            };

            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(500) as usize;
            let offset = args
                .get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let include_groups = args
                .get("include_groups")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let total = match store.chat_count(include_groups) {
                Ok(t) => t,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to count chats: {e}")),
                    });
                }
            };

            match store.list_chats(limit, offset, include_groups) {
                Ok(chats) => {
                    let returned = chats.len();
                    let chat_list: Vec<serde_json::Value> = chats
                        .into_iter()
                        .map(|c| {
                            json!({
                                "id": c.chat_id,
                                "name": c.chat_name,
                                "chat_type": if c.is_group { "group" } else { "individual" },
                                "is_group": c.is_group,
                                "last_message_time": chrono::DateTime::from_timestamp(
                                    c.last_message_time, 0
                                )
                                .map(|dt| dt.to_rfc3339())
                                .unwrap_or_else(|| c.last_message_time.to_string()),
                                "last_message_time_unix": c.last_message_time,
                                "message_count": c.message_count,
                                "unread_count": c.unread_count,
                            })
                        })
                        .collect();

                    let has_more = (offset + returned) < total as usize;
                    let result = json!({
                        "chats": chat_list,
                        "returned": returned,
                        "total": total,
                        "offset": offset,
                        "limit": limit,
                        "has_more": has_more,
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
                    error: Some(format!("Failed to list chats: {e}")),
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
        let tool = WhatsAppListChatsTool::new(
            Arc::new(SecurityPolicy::default()),
            std::path::Path::new("/tmp"),
        );
        assert_eq!(tool.name(), "whatsapp_list_chats");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        // Pagination parameters must be advertised.
        assert!(schema["properties"]["offset"].is_object());
        assert!(schema["properties"]["limit"].is_object());
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn execute_returns_structured_json_from_seeded_store() {
        use crate::channels::whatsapp_message_store::{StoredMessage, WhatsAppMessageStore};

        let tmp = tempfile::TempDir::new().unwrap();
        // Seed the store at the path the tool will open.
        {
            let store =
                WhatsAppMessageStore::new(&tmp.path().join("whatsapp-messages.db")).unwrap();
            for i in 0..3 {
                store
                    .upsert_message(&StoredMessage {
                        id: format!("msg_{i}"),
                        chat_id: format!("100000000{i}@s.whatsapp.net"),
                        chat_name: format!("Contact {i}"),
                        sender_name: format!("Contact {i}"),
                        from_me: false,
                        body: "hi".into(),
                        timestamp: 1_700_000_000 + i as i64,
                        has_media: false,
                        msg_type: "chat".into(),
                    })
                    .unwrap();
            }
        }

        let tool =
            WhatsAppListChatsTool::new(Arc::new(SecurityPolicy::default()), tmp.path());
        let result = tool.execute(json!({ "limit": 2, "offset": 0 })).await.unwrap();
        assert!(result.success, "{:?}", result.error);

        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["returned"], 2);
        assert_eq!(parsed["total"], 3);
        assert_eq!(parsed["has_more"], true);
        assert!(parsed["chats"].as_array().unwrap()[0]["chat_type"]
            .as_str()
            .unwrap()
            .eq("individual"));
    }
}
