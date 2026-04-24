//! WhatsApp message persistence layer for mining tools.
//!
//! Stores incoming and history-sync messages in SQLite so that agent tools
//! (whatsapp_list_chats, whatsapp_read_history, whatsapp_get_contact) can
//! query conversation history.
//!
//! # Privacy
//!
//! Messages stored here are L2 (private, local only). The database never
//! leaves the Mac Mini. Phone numbers are stored for contact matching but
//! must never appear in extracted facts or summaries.

#[cfg(feature = "whatsapp-web")]
use parking_lot::Mutex;
#[cfg(feature = "whatsapp-web")]
use rusqlite::{params, Connection};
#[cfg(feature = "whatsapp-web")]
use std::path::Path;
#[cfg(feature = "whatsapp-web")]
use std::sync::Arc;

/// A single stored WhatsApp message.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredMessage {
    pub id: String,
    pub chat_id: String,
    pub chat_name: String,
    pub sender_name: String,
    pub from_me: bool,
    pub body: String,
    pub timestamp: i64,
    pub has_media: bool,
    pub msg_type: String,
}

/// Summary of a chat for listing.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatSummary {
    pub chat_id: String,
    pub chat_name: String,
    pub is_group: bool,
    pub last_message_time: i64,
    pub unread_count: i64,
    pub message_count: i64,
}

/// Contact info stored alongside messages.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredContact {
    pub chat_id: String,
    pub name: String,
    pub push_name: String,
    pub phone_number: String,
    pub is_group: bool,
}

/// SQLite-backed message store for WhatsApp mining.
#[cfg(feature = "whatsapp-web")]
pub struct WhatsAppMessageStore {
    conn: Arc<Mutex<Connection>>,
}

#[cfg(feature = "whatsapp-web")]
impl WhatsAppMessageStore {
    /// Open (or create) the message store at the given path.
    pub fn new(db_path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                id          TEXT PRIMARY KEY,
                chat_id     TEXT NOT NULL,
                chat_name   TEXT NOT NULL DEFAULT '',
                sender_name TEXT NOT NULL DEFAULT '',
                from_me     INTEGER NOT NULL DEFAULT 0,
                body        TEXT NOT NULL DEFAULT '',
                timestamp   INTEGER NOT NULL,
                has_media   INTEGER NOT NULL DEFAULT 0,
                msg_type    TEXT NOT NULL DEFAULT 'chat'
            );

            CREATE INDEX IF NOT EXISTS idx_messages_chat_ts
                ON messages(chat_id, timestamp DESC);

            CREATE TABLE IF NOT EXISTS contacts (
                chat_id      TEXT PRIMARY KEY,
                name         TEXT NOT NULL DEFAULT '',
                push_name    TEXT NOT NULL DEFAULT '',
                phone_number TEXT NOT NULL DEFAULT '',
                is_group     INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS chat_meta (
                chat_id         TEXT PRIMARY KEY,
                chat_name       TEXT NOT NULL DEFAULT '',
                is_group        INTEGER NOT NULL DEFAULT 0,
                last_message_ts INTEGER NOT NULL DEFAULT 0,
                unread_count    INTEGER NOT NULL DEFAULT 0
            );",
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert or update a message.
    pub fn upsert_message(&self, msg: &StoredMessage) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO messages
                (id, chat_id, chat_name, sender_name, from_me, body, timestamp, has_media, msg_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                msg.id,
                msg.chat_id,
                msg.chat_name,
                msg.sender_name,
                msg.from_me as i32,
                msg.body,
                msg.timestamp,
                msg.has_media as i32,
                msg.msg_type,
            ],
        )?;

        // Update chat metadata
        conn.execute(
            "INSERT INTO chat_meta (chat_id, chat_name, is_group, last_message_ts, unread_count)
             VALUES (?1, ?2, ?3, ?4, 0)
             ON CONFLICT(chat_id) DO UPDATE SET
                chat_name = CASE WHEN ?2 != '' THEN ?2 ELSE chat_name END,
                last_message_ts = MAX(last_message_ts, ?4)",
            params![
                msg.chat_id,
                msg.chat_name,
                msg.chat_id.contains("@g.us") as i32,
                msg.timestamp,
            ],
        )?;
        Ok(())
    }

    /// Update or insert contact info.
    pub fn upsert_contact(&self, contact: &StoredContact) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO contacts
                (chat_id, name, push_name, phone_number, is_group)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                contact.chat_id,
                contact.name,
                contact.push_name,
                contact.phone_number,
                contact.is_group as i32,
            ],
        )?;
        Ok(())
    }

    /// List chats sorted by most recent message.
    pub fn list_chats(&self, limit: usize, include_groups: bool) -> anyhow::Result<Vec<ChatSummary>> {
        let conn = self.conn.lock();
        let mut stmt = if include_groups {
            conn.prepare(
                "SELECT cm.chat_id, cm.chat_name, cm.is_group, cm.last_message_ts, cm.unread_count,
                        (SELECT COUNT(*) FROM messages m WHERE m.chat_id = cm.chat_id) as msg_count
                 FROM chat_meta cm
                 ORDER BY cm.last_message_ts DESC
                 LIMIT ?1"
            )?
        } else {
            conn.prepare(
                "SELECT cm.chat_id, cm.chat_name, cm.is_group, cm.last_message_ts, cm.unread_count,
                        (SELECT COUNT(*) FROM messages m WHERE m.chat_id = cm.chat_id) as msg_count
                 FROM chat_meta cm
                 WHERE cm.is_group = 0
                 ORDER BY cm.last_message_ts DESC
                 LIMIT ?1"
            )?
        };

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(ChatSummary {
                chat_id: row.get(0)?,
                chat_name: row.get(1)?,
                is_group: row.get::<_, i32>(2)? != 0,
                last_message_time: row.get(3)?,
                unread_count: row.get(4)?,
                message_count: row.get(5)?,
            })
        })?;

        let mut chats = Vec::new();
        for row in rows {
            chats.push(row?);
        }
        Ok(chats)
    }

    /// Read messages from a specific chat.
    pub fn read_history(
        &self,
        chat_id: &str,
        limit: usize,
        since_ts: Option<i64>,
    ) -> anyhow::Result<(Vec<StoredMessage>, String)> {
        let conn = self.conn.lock();

        // Get chat name
        let chat_name: String = conn
            .query_row(
                "SELECT chat_name FROM chat_meta WHERE chat_id = ?1",
                params![chat_id],
                |row| row.get(0),
            )
            .unwrap_or_default();

        let mut stmt = if let Some(ts) = since_ts {
            let mut s = conn.prepare(
                "SELECT id, chat_id, chat_name, sender_name, from_me, body, timestamp, has_media, msg_type
                 FROM messages
                 WHERE chat_id = ?1 AND timestamp > ?2
                 ORDER BY timestamp ASC
                 LIMIT ?3"
            )?;
            let rows = s.query_map(params![chat_id, ts, limit as i64], Self::row_to_message)?;
            let mut msgs = Vec::new();
            for row in rows {
                msgs.push(row?);
            }
            return Ok((msgs, chat_name));
        } else {
            conn.prepare(
                "SELECT id, chat_id, chat_name, sender_name, from_me, body, timestamp, has_media, msg_type
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY timestamp DESC
                 LIMIT ?2"
            )?
        };

        let rows = stmt.query_map(params![chat_id, limit as i64], Self::row_to_message)?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        // Reverse to chronological order
        msgs.reverse();
        Ok((msgs, chat_name))
    }

    /// Look up a chat_id by contact name (fuzzy match).
    pub fn find_chat_by_name(&self, name: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock();
        let result: Option<String> = conn
            .query_row(
                "SELECT chat_id FROM chat_meta WHERE chat_name LIKE ?1 LIMIT 1",
                params![format!("%{name}%")],
                |row| row.get(0),
            )
            .ok();
        Ok(result)
    }

    /// Get contact info for a chat.
    pub fn get_contact(&self, chat_id: &str) -> anyhow::Result<Option<StoredContact>> {
        let conn = self.conn.lock();
        let result = conn
            .query_row(
                "SELECT chat_id, name, push_name, phone_number, is_group
                 FROM contacts WHERE chat_id = ?1",
                params![chat_id],
                |row| {
                    Ok(StoredContact {
                        chat_id: row.get(0)?,
                        name: row.get(1)?,
                        push_name: row.get(2)?,
                        phone_number: row.get(3)?,
                        is_group: row.get::<_, i32>(4)? != 0,
                    })
                },
            )
            .ok();
        Ok(result)
    }

    /// Total message count.
    pub fn message_count(&self) -> anyhow::Result<i64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
        Ok(count)
    }

    fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<StoredMessage> {
        Ok(StoredMessage {
            id: row.get(0)?,
            chat_id: row.get(1)?,
            chat_name: row.get(2)?,
            sender_name: row.get(3)?,
            from_me: row.get::<_, i32>(4)? != 0,
            body: row.get(5)?,
            timestamp: row.get(6)?,
            has_media: row.get::<_, i32>(7)? != 0,
            msg_type: row.get(8)?,
        })
    }
}

// Stub when feature is disabled
#[cfg(not(feature = "whatsapp-web"))]
pub struct WhatsAppMessageStore;

#[cfg(not(feature = "whatsapp-web"))]
impl WhatsAppMessageStore {
    pub fn new(_db_path: &std::path::Path) -> anyhow::Result<Self> {
        anyhow::bail!("WhatsApp message store requires the 'whatsapp-web' feature");
    }
}

#[cfg(test)]
#[cfg(feature = "whatsapp-web")]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (WhatsAppMessageStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = WhatsAppMessageStore::new(&tmp.path().join("wa-messages.db")).unwrap();
        (store, tmp)
    }

    #[test]
    fn upsert_and_list_chats() {
        let (store, _tmp) = test_store();

        store.upsert_message(&StoredMessage {
            id: "msg_1".into(),
            chat_id: "447977553025@s.whatsapp.net".into(),
            chat_name: "Test Contact".into(),
            sender_name: "Test Contact".into(),
            from_me: false,
            body: "Hello!".into(),
            timestamp: 1000,
            has_media: false,
            msg_type: "chat".into(),
        }).unwrap();

        let chats = store.list_chats(10, false).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].chat_name, "Test Contact");
        assert_eq!(chats[0].message_count, 1);
    }

    #[test]
    fn read_history_chronological() {
        let (store, _tmp) = test_store();
        let chat_id = "12345@s.whatsapp.net";

        for i in 0..5 {
            store.upsert_message(&StoredMessage {
                id: format!("msg_{i}"),
                chat_id: chat_id.into(),
                chat_name: "Alice".into(),
                sender_name: if i % 2 == 0 { "Alice" } else { "Andy" }.into(),
                from_me: i % 2 != 0,
                body: format!("Message {i}"),
                timestamp: 1000 + i,
                has_media: false,
                msg_type: "chat".into(),
            }).unwrap();
        }

        let (msgs, name) = store.read_history(chat_id, 50, None).unwrap();
        assert_eq!(msgs.len(), 5);
        assert_eq!(name, "Alice");
        // Should be in chronological order
        assert!(msgs[0].timestamp < msgs[4].timestamp);
    }

    #[test]
    fn read_history_since_timestamp() {
        let (store, _tmp) = test_store();
        let chat_id = "12345@s.whatsapp.net";

        for i in 0..10 {
            store.upsert_message(&StoredMessage {
                id: format!("msg_{i}"),
                chat_id: chat_id.into(),
                chat_name: "Bob".into(),
                sender_name: "Bob".into(),
                from_me: false,
                body: format!("Message {i}"),
                timestamp: 1000 + i,
                has_media: false,
                msg_type: "chat".into(),
            }).unwrap();
        }

        let (msgs, _) = store.read_history(chat_id, 50, Some(1005)).unwrap();
        assert_eq!(msgs.len(), 4); // messages 6,7,8,9
    }

    #[test]
    fn filter_groups() {
        let (store, _tmp) = test_store();

        // Individual chat
        store.upsert_message(&StoredMessage {
            id: "msg_dm".into(),
            chat_id: "12345@s.whatsapp.net".into(),
            chat_name: "Alice".into(),
            sender_name: "Alice".into(),
            from_me: false,
            body: "DM".into(),
            timestamp: 2000,
            has_media: false,
            msg_type: "chat".into(),
        }).unwrap();

        // Group chat
        store.upsert_message(&StoredMessage {
            id: "msg_group".into(),
            chat_id: "98765@g.us".into(),
            chat_name: "Work Group".into(),
            sender_name: "Bob".into(),
            from_me: false,
            body: "Group msg".into(),
            timestamp: 3000,
            has_media: false,
            msg_type: "chat".into(),
        }).unwrap();

        let all = store.list_chats(10, true).unwrap();
        assert_eq!(all.len(), 2);

        let individual = store.list_chats(10, false).unwrap();
        assert_eq!(individual.len(), 1);
        assert_eq!(individual[0].chat_name, "Alice");
    }

    #[test]
    fn find_chat_by_name_fuzzy() {
        let (store, _tmp) = test_store();

        store.upsert_message(&StoredMessage {
            id: "msg_1".into(),
            chat_id: "12345@s.whatsapp.net".into(),
            chat_name: "David Chen".into(),
            sender_name: "David Chen".into(),
            from_me: false,
            body: "Hi".into(),
            timestamp: 1000,
            has_media: false,
            msg_type: "chat".into(),
        }).unwrap();

        assert!(store.find_chat_by_name("David").unwrap().is_some());
        assert!(store.find_chat_by_name("Chen").unwrap().is_some());
        assert!(store.find_chat_by_name("unknown").unwrap().is_none());
    }

    #[test]
    fn contact_upsert_and_get() {
        let (store, _tmp) = test_store();

        store.upsert_contact(&StoredContact {
            chat_id: "12345@s.whatsapp.net".into(),
            name: "David Chen".into(),
            push_name: "David".into(),
            phone_number: "+852XXXXXXXX".into(),
            is_group: false,
        }).unwrap();

        let contact = store.get_contact("12345@s.whatsapp.net").unwrap().unwrap();
        assert_eq!(contact.name, "David Chen");
        assert_eq!(contact.push_name, "David");
    }
}
