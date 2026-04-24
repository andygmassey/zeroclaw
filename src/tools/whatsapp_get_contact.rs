//! Tool: whatsapp_get_contact
//!
//! Gets contact details for a WhatsApp user by chat ID.

use super::traits::{Tool, ToolResult};
use crate::channels::whatsapp_message_store::WhatsAppMessageStore;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Agent-callable tool for looking up WhatsApp contact details.
pub struct WhatsAppGetContactTool {
    store_path: PathBuf,
    security: Arc<SecurityPolicy>,
}

impl WhatsAppGetContactTool {
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
impl Tool for WhatsAppGetContactTool {
    fn name(&self) -> &str {
        "whatsapp_get_contact"
    }

    fn description(&self) -> &str {
        "Get contact details for a WhatsApp user. \
         Provide the chat_id from whatsapp_list_chats, or a contact name. \
         Returns name, push name, phone number, and whether it's a group."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "Chat ID or contact name to look up"
                }
            },
            "required": ["chat_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "whatsapp_get_contact")
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
                        error: Some(format!("Failed to open WhatsApp message store: {e}")),
                    });
                }
            };

            let raw_id = args
                .get("chat_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'chat_id' parameter"))?;

            // Resolve to JID if needed
            let chat_id = if raw_id.contains('@') {
                raw_id.to_string()
            } else {
                match store.find_chat_by_name(raw_id) {
                    Ok(Some(id)) => id,
                    Ok(None) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!(
                                "No contact found matching '{raw_id}'. \
                                 Use whatsapp_list_chats to see available contacts."
                            )),
                        });
                    }
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Failed to search: {e}")),
                        });
                    }
                }
            };

            match store.get_contact(&chat_id) {
                Ok(Some(contact)) => {
                    let result = json!({
                        "id": contact.chat_id,
                        "name": contact.name,
                        "push_name": contact.push_name,
                        "phone_number": contact.phone_number,
                        "is_group": contact.is_group,
                    });

                    Ok(ToolResult {
                        success: true,
                        output: serde_json::to_string_pretty(&result).unwrap_or_default(),
                        error: None,
                    })
                }
                Ok(None) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "No contact details stored for '{chat_id}'. \
                         Contact info is populated when messages are received."
                    )),
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to get contact: {e}")),
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
        let tool = WhatsAppGetContactTool::new(
            Arc::new(SecurityPolicy::default()),
            std::path::Path::new("/tmp"),
        );
        assert_eq!(tool.name(), "whatsapp_get_contact");
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "chat_id"));
    }
}
