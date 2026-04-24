//! WhatsApp message persistence layer for mining tools.
//!
//! Stores incoming and history-sync messages in SQLite so that agent tools
//! ([`WhatsAppListChatsTool`](crate::tools::WhatsAppListChatsTool),
//!  [`WhatsAppReadHistoryTool`](crate::tools::WhatsAppReadHistoryTool),
//!  [`WhatsAppGetContactTool`](crate::tools::WhatsAppGetContactTool))
//! can query conversation history.
//!
//! # Schema
//!
//! Three tables, all idempotently created on open:
//!
//! - `messages` – one row per WhatsApp message, keyed by the platform message id
//! - `contacts` – one row per JID with optional `last_seen` / `profile_photo_url`
//! - `chat_meta` – denormalised per-chat rollup used by the list-chats query
//!
//! Schema migrations are additive and safe to replay. New columns are added via
//! `ALTER TABLE` guarded by a `PRAGMA table_info` check so older databases open
//! cleanly.
//!
//! # Privacy
//!
//! Messages stored here are L2 (private, local only). The database never leaves
//! the host. Phone numbers are stored for contact matching but must never appear
//! in extracted facts, summaries, or any output that leaves the device.

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
///
/// `last_seen` and `profile_photo_url` are optional because WhatsApp Web does
/// not always expose them (privacy controls vary per contact). We never fetch
/// anything that is not already visible to the linked session.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredContact {
    pub chat_id: String,
    pub name: String,
    pub push_name: String,
    pub phone_number: String,
    pub is_group: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_photo_url: Option<String>,
}

#[cfg(feature = "whatsapp-web")]
impl StoredContact {
    /// Construct a contact with the minimum required fields. Optional fields
    /// default to `None` and can be filled in with the builder-style setters.
    pub fn new(chat_id: impl Into<String>, name: impl Into<String>, is_group: bool) -> Self {
        Self {
            chat_id: chat_id.into(),
            name: name.into(),
            push_name: String::new(),
            phone_number: String::new(),
            is_group,
            last_seen: None,
            profile_photo_url: None,
        }
    }
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

        // Additive migrations – safe to replay. Each checks PRAGMA table_info
        // for the target column and only issues ALTER TABLE when absent.
        Self::ensure_column(&conn, "contacts", "last_seen", "INTEGER")?;
        Self::ensure_column(&conn, "contacts", "profile_photo_url", "TEXT")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Add `column` of `col_type` to `table` if it does not already exist.
    fn ensure_column(
        conn: &Connection,
        table: &str,
        column: &str,
        col_type: &str,
    ) -> anyhow::Result<()> {
        let pragma = format!("PRAGMA table_info({table})");
        let mut stmt = conn.prepare(&pragma)?;
        let existing: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .collect();
        if !existing.iter().any(|c| c == column) {
            let alter = format!("ALTER TABLE {table} ADD COLUMN {column} {col_type}");
            conn.execute(&alter, [])?;
        }
        Ok(())
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
            "INSERT INTO contacts
                (chat_id, name, push_name, phone_number, is_group, last_seen, profile_photo_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(chat_id) DO UPDATE SET
                name              = CASE WHEN ?2 != '' THEN ?2 ELSE contacts.name END,
                push_name         = CASE WHEN ?3 != '' THEN ?3 ELSE contacts.push_name END,
                phone_number      = CASE WHEN ?4 != '' THEN ?4 ELSE contacts.phone_number END,
                is_group          = ?5,
                last_seen         = COALESCE(?6, contacts.last_seen),
                profile_photo_url = COALESCE(?7, contacts.profile_photo_url)",
            params![
                contact.chat_id,
                contact.name,
                contact.push_name,
                contact.phone_number,
                contact.is_group as i32,
                contact.last_seen,
                contact.profile_photo_url,
            ],
        )?;
        Ok(())
    }

    /// List chats sorted by most recent message.
    ///
    /// Results are sorted by `last_message_ts DESC`. `offset` is applied after
    /// the sort, so callers can paginate with stable ordering while the store
    /// is quiescent. Re-ordering under heavy ingestion is expected; pagination
    /// is not a snapshot.
    pub fn list_chats(
        &self,
        limit: usize,
        offset: usize,
        include_groups: bool,
    ) -> anyhow::Result<Vec<ChatSummary>> {
        let conn = self.conn.lock();
        let sql_base = "SELECT cm.chat_id, cm.chat_name, cm.is_group, cm.last_message_ts, cm.unread_count,
                               (SELECT COUNT(*) FROM messages m WHERE m.chat_id = cm.chat_id) as msg_count
                        FROM chat_meta cm";
        let where_clause = if include_groups { "" } else { " WHERE cm.is_group = 0" };
        let full_sql = format!(
            "{sql_base}{where_clause}
             ORDER BY cm.last_message_ts DESC
             LIMIT ?1 OFFSET ?2"
        );

        let mut stmt = conn.prepare(&full_sql)?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
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
    ///
    /// When `since_ts` is `None`, returns the most recent `limit` messages in
    /// chronological (ascending) order. When `since_ts` is provided, returns
    /// the next `limit` messages strictly after that timestamp, also in
    /// chronological order – suited to incremental polling from a cursor.
    pub fn read_history(
        &self,
        chat_id: &str,
        limit: usize,
        since_ts: Option<i64>,
    ) -> anyhow::Result<(Vec<StoredMessage>, String)> {
        let conn = self.conn.lock();

        // Get chat name (empty if no chat_meta row yet)
        let chat_name: String = conn
            .query_row(
                "SELECT chat_name FROM chat_meta WHERE chat_id = ?1",
                params![chat_id],
                |row| row.get(0),
            )
            .unwrap_or_default();

        let mut msgs = if let Some(ts) = since_ts {
            let mut stmt = conn.prepare(
                "SELECT id, chat_id, chat_name, sender_name, from_me, body, timestamp, has_media, msg_type
                 FROM messages
                 WHERE chat_id = ?1 AND timestamp > ?2
                 ORDER BY timestamp ASC
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                params![chat_id, ts, limit as i64],
                Self::row_to_message,
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, chat_id, chat_name, sender_name, from_me, body, timestamp, has_media, msg_type
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY timestamp DESC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![chat_id, limit as i64], Self::row_to_message)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            // Fetched newest-first; reverse for chronological presentation.
            out.reverse();
            out
        };

        // Defensive: re-sort ascending in case duplicates or out-of-order inserts.
        msgs.sort_by_key(|m| m.timestamp);
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
                "SELECT chat_id, name, push_name, phone_number, is_group,
                        last_seen, profile_photo_url
                 FROM contacts WHERE chat_id = ?1",
                params![chat_id],
                |row| {
                    Ok(StoredContact {
                        chat_id: row.get(0)?,
                        name: row.get(1)?,
                        push_name: row.get(2)?,
                        phone_number: row.get(3)?,
                        is_group: row.get::<_, i32>(4)? != 0,
                        last_seen: row.get(5).ok(),
                        profile_photo_url: row.get(6).ok(),
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

    /// Total chat count (optionally filtered to exclude groups).
    pub fn chat_count(&self, include_groups: bool) -> anyhow::Result<i64> {
        let conn = self.conn.lock();
        let sql = if include_groups {
            "SELECT COUNT(*) FROM chat_meta"
        } else {
            "SELECT COUNT(*) FROM chat_meta WHERE is_group = 0"
        };
        let count: i64 = conn.query_row(sql, [], |row| row.get(0))?;
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

        // Synthetic JID – never a real phone number.
        store.upsert_message(&StoredMessage {
            id: "msg_1".into(),
            chat_id: "1234567890@s.whatsapp.net".into(),
            chat_name: "Test Contact".into(),
            sender_name: "Test Contact".into(),
            from_me: false,
            body: "Hello!".into(),
            timestamp: 1000,
            has_media: false,
            msg_type: "chat".into(),
        }).unwrap();

        let chats = store.list_chats(10, 0, false).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].chat_name, "Test Contact");
        assert_eq!(chats[0].message_count, 1);
    }

    #[test]
    fn list_chats_pagination() {
        let (store, _tmp) = test_store();

        // Insert 5 chats with distinct, increasing timestamps so ordering is
        // deterministic (newest first when listed).
        for i in 0..5 {
            store.upsert_message(&StoredMessage {
                id: format!("msg_{i}"),
                chat_id: format!("100000000{i}@s.whatsapp.net"),
                chat_name: format!("Contact {i}"),
                sender_name: format!("Contact {i}"),
                from_me: false,
                body: "Hi".into(),
                timestamp: 1000 + i as i64,
                has_media: false,
                msg_type: "chat".into(),
            }).unwrap();
        }

        let page1 = store.list_chats(2, 0, false).unwrap();
        let page2 = store.list_chats(2, 2, false).unwrap();
        let page3 = store.list_chats(2, 4, false).unwrap();

        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 2);
        assert_eq!(page3.len(), 1);
        // All pages together should match a single full listing.
        assert_eq!(store.chat_count(false).unwrap(), 5);

        // Pages should be disjoint.
        let ids: Vec<_> = page1.iter().chain(&page2).chain(&page3)
            .map(|c| c.chat_id.clone())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len(), "pages must not overlap");
    }

    #[test]
    fn read_history_chronological() {
        let (store, _tmp) = test_store();
        let chat_id = "1234567890@s.whatsapp.net";

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
        let chat_id = "1234567890@s.whatsapp.net";

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
        // Still chronological.
        assert!(msgs.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
    }

    #[test]
    fn read_history_respects_limit_when_more_available() {
        let (store, _tmp) = test_store();
        let chat_id = "1234567890@s.whatsapp.net";

        for i in 0..20 {
            store.upsert_message(&StoredMessage {
                id: format!("msg_{i}"),
                chat_id: chat_id.into(),
                chat_name: "Carol".into(),
                sender_name: "Carol".into(),
                from_me: false,
                body: format!("Message {i}"),
                timestamp: 1000 + i,
                has_media: false,
                msg_type: "chat".into(),
            }).unwrap();
        }

        let (msgs, _) = store.read_history(chat_id, 5, None).unwrap();
        assert_eq!(msgs.len(), 5);
        // Most-recent-5 returned, still ascending.
        let timestamps: Vec<_> = msgs.iter().map(|m| m.timestamp).collect();
        assert_eq!(timestamps, vec![1015, 1016, 1017, 1018, 1019]);
    }

    #[test]
    fn filter_groups() {
        let (store, _tmp) = test_store();

        // Individual chat – synthetic JID.
        store.upsert_message(&StoredMessage {
            id: "msg_dm".into(),
            chat_id: "1234567890@s.whatsapp.net".into(),
            chat_name: "Alice".into(),
            sender_name: "Alice".into(),
            from_me: false,
            body: "DM".into(),
            timestamp: 2000,
            has_media: false,
            msg_type: "chat".into(),
        }).unwrap();

        // Group chat – synthetic group JID.
        store.upsert_message(&StoredMessage {
            id: "msg_group".into(),
            chat_id: "9876543210@g.us".into(),
            chat_name: "Work Group".into(),
            sender_name: "Bob".into(),
            from_me: false,
            body: "Group msg".into(),
            timestamp: 3000,
            has_media: false,
            msg_type: "chat".into(),
        }).unwrap();

        let all = store.list_chats(10, 0, true).unwrap();
        assert_eq!(all.len(), 2);

        let individual = store.list_chats(10, 0, false).unwrap();
        assert_eq!(individual.len(), 1);
        assert_eq!(individual[0].chat_name, "Alice");
    }

    #[test]
    fn find_chat_by_name_fuzzy() {
        let (store, _tmp) = test_store();

        store.upsert_message(&StoredMessage {
            id: "msg_1".into(),
            chat_id: "1234567890@s.whatsapp.net".into(),
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
            chat_id: "1234567890@s.whatsapp.net".into(),
            name: "David Chen".into(),
            push_name: "David".into(),
            phone_number: "+44 7000 000000".into(),
            is_group: false,
            last_seen: None,
            profile_photo_url: None,
        }).unwrap();

        let contact = store.get_contact("1234567890@s.whatsapp.net").unwrap().unwrap();
        assert_eq!(contact.name, "David Chen");
        assert_eq!(contact.push_name, "David");
        assert_eq!(contact.last_seen, None);
        assert_eq!(contact.profile_photo_url, None);
    }

    #[test]
    fn contact_upsert_preserves_optional_fields_on_merge() {
        let (store, _tmp) = test_store();
        let jid = "1234567890@s.whatsapp.net";

        // First write: populate last_seen and profile photo.
        store.upsert_contact(&StoredContact {
            chat_id: jid.into(),
            name: "Eve".into(),
            push_name: "Eve".into(),
            phone_number: "+44 7000 000001".into(),
            is_group: false,
            last_seen: Some(1_700_000_000),
            profile_photo_url: Some("https://example.invalid/a.jpg".into()),
        }).unwrap();

        // Second write: updated push_name only; optional fields must persist.
        store.upsert_contact(&StoredContact {
            chat_id: jid.into(),
            name: "Eve".into(),
            push_name: "Evelyn".into(),
            phone_number: "+44 7000 000001".into(),
            is_group: false,
            last_seen: None,
            profile_photo_url: None,
        }).unwrap();

        let contact = store.get_contact(jid).unwrap().unwrap();
        assert_eq!(contact.push_name, "Evelyn");
        assert_eq!(contact.last_seen, Some(1_700_000_000));
        assert_eq!(contact.profile_photo_url.as_deref(), Some("https://example.invalid/a.jpg"));
    }

    #[test]
    fn schema_migration_idempotent_on_reopen() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("wa-messages.db");

        // First open runs the full schema + migrations.
        let store_a = WhatsAppMessageStore::new(&path).unwrap();
        store_a.upsert_contact(&StoredContact {
            chat_id: "1234567890@s.whatsapp.net".into(),
            name: "Frank".into(),
            push_name: "Frank".into(),
            phone_number: "+44 7000 000002".into(),
            is_group: false,
            last_seen: Some(1_700_000_000),
            profile_photo_url: None,
        }).unwrap();
        drop(store_a);

        // Re-opening with existing DB must be a no-op for migrations.
        let store_b = WhatsAppMessageStore::new(&path).unwrap();
        let contact = store_b.get_contact("1234567890@s.whatsapp.net").unwrap().unwrap();
        assert_eq!(contact.last_seen, Some(1_700_000_000));
    }
}
