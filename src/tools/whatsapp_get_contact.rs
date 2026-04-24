//! Tool: `whatsapp_get_contact`.
//!
//! Looks up a contact's stored profile info: display name, push name, phone
//! number (as stored by the session), group flag, last-seen epoch (if the
//! contact's privacy settings expose it), and profile photo URL (if any).
//!
//! # Privacy
//!
//! This tool only exposes data already persisted by the WhatsApp Web channel.
//! It never makes a fresh server round trip, never decrypts anything not
//! already decrypted by the session, and never returns data the linked
//! WhatsApp Web session cannot already see.
//!
//! # Inputs
//!
//! - `chat_id` (string, required) – JID or a contact-name substring.
//!
//! # Output
//!
//! ```json
//! {
//!   "id": "1234567890@s.whatsapp.net",
//!   "name": "Ada Lovelace",
//!   "push_name": "Ada",
//!   "phone_number": "+44 7000 000000",
//!   "is_group": false,
//!   "chat_type": "individual",
//!   "last_seen": "2026-04-24T08:12:01+00:00",
//!   "last_seen_unix": 1714032721,
//!   "profile_photo_url": null
//! }
//! ```
//!
//! When a field is unknown the corresponding JSON key is `null`.

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
        "Get stored profile info for a WhatsApp contact. \
         Accepts a chat_id (JID) from whatsapp_list_chats, or a contact name \
         (fuzzy-matched). Returns only fields the linked WhatsApp Web session \
         already has – no fresh server calls, no data exposed beyond what \
         WhatsApp itself exposes."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "Chat JID or a contact name substring to look up"
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
                    let last_seen_iso = contact.last_seen.and_then(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).map(|dt| dt.to_rfc3339())
                    });
                    let result = json!({
                        "id": contact.chat_id,
                        "name": contact.name,
                        "push_name": contact.push_name,
                        "phone_number": contact.phone_number,
                        "is_group": contact.is_group,
                        "chat_type": if contact.is_group { "group" } else { "individual" },
                        "last_seen": last_seen_iso,
                        "last_seen_unix": contact.last_seen,
                        "profile_photo_url": contact.profile_photo_url,
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

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn execute_returns_full_contact_record() {
        use crate::channels::whatsapp_message_store::{StoredContact, WhatsAppMessageStore};

        let tmp = tempfile::TempDir::new().unwrap();
        let jid = "1234567890@s.whatsapp.net";
        {
            let store =
                WhatsAppMessageStore::new(&tmp.path().join("whatsapp-messages.db")).unwrap();
            store
                .upsert_contact(&StoredContact {
                    chat_id: jid.into(),
                    name: "Ada Lovelace".into(),
                    push_name: "Ada".into(),
                    // Synthetic UK test number per CM047 brief.
                    phone_number: "+44 7000 000000".into(),
                    is_group: false,
                    last_seen: Some(1_714_032_721),
                    profile_photo_url: None,
                })
                .unwrap();
        }

        let tool =
            WhatsAppGetContactTool::new(Arc::new(SecurityPolicy::default()), tmp.path());
        let result = tool.execute(json!({ "chat_id": jid })).await.unwrap();
        assert!(result.success, "{:?}", result.error);

        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["id"], jid);
        assert_eq!(parsed["name"], "Ada Lovelace");
        assert_eq!(parsed["chat_type"], "individual");
        assert_eq!(parsed["last_seen_unix"], 1_714_032_721_i64);
        assert!(parsed["last_seen"].is_string());
        assert!(parsed["profile_photo_url"].is_null());
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn execute_reports_missing_contact_clearly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool =
            WhatsAppGetContactTool::new(Arc::new(SecurityPolicy::default()), tmp.path());
        let result = tool
            .execute(json!({ "chat_id": "1234567890@s.whatsapp.net" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("No contact details stored"));
    }
}
