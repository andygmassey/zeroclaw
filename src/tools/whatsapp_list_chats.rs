//! Tool: whatsapp_list_chats
//!
//! Lists WhatsApp conversations sorted by most recent activity.
//! Opens the shared WhatsApp message store at ~/.zeroclaw/whatsapp-messages.db.

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
         Returns chat name, last message time, and message count. \
         By default only shows 1:1 chats (set include_groups=true for group chats)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum chats to return (default 20)",
                    "default": 20
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
                .unwrap_or(20) as usize;
            let include_groups = args
                .get("include_groups")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            match store.list_chats(limit, include_groups) {
                Ok(chats) => {
                    let total = chats.len();
                    let chat_list: Vec<serde_json::Value> = chats
                        .into_iter()
                        .map(|c| {
                            json!({
                                "id": c.chat_id,
                                "name": c.chat_name,
                                "is_group": c.is_group,
                                "last_message_time": chrono::DateTime::from_timestamp(
                                    c.last_message_time, 0
                                )
                                .map(|dt| dt.to_rfc3339())
                                .unwrap_or_else(|| c.last_message_time.to_string()),
                                "message_count": c.message_count,
                                "unread_count": c.unread_count,
                            })
                        })
                        .collect();

                    let result = json!({
                        "chats": chat_list,
                        "returned": total,
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
    }
}
