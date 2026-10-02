use crate::secrets;
use crate::types::{
    ChatMessage, Conversation, Memory, MemoryPatch, ModelConfig, ModelSettings, Project,
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub struct DbState {
    pub conn: Mutex<Connection>,
}

const SCHEMA_VERSION: &str = "4";
const FTS_SCHEMA_VERSION: &str = "2";

pub fn now() -> String {
    Utc::now().to_rfc3339()
}

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn init_database(app: &AppHandle) -> Result<Connection, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Cannot resolve app data dir: {error}"))?;
    fs::create_dir_all(&data_dir)
        .map_err(|error| format!("Cannot create app data dir: {error}"))?;
    let db_path = data_dir.join("mira.sqlite3");
    let conn = Connection::open(db_path)
        .map_err(|error| format!("Cannot open SQLite database: {error}"))?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS schema_meta(
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS projects(
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            is_archived INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS conversations(
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            project_id TEXT,
            is_archived INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS messages(
            id TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS model_configs(
            id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            name TEXT NOT NULL,
            base_url TEXT NOT NULL,
            model TEXT NOT NULL,
            api_key TEXT,
            is_default INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS memories(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fact TEXT NOT NULL,
            memory_type TEXT,
            importance INTEGER DEFAULT 5,
            confidence REAL DEFAULT 1.0,
            tags TEXT,
            source_conversation_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            last_used_at TEXT,
            use_count INTEGER DEFAULT 0,
            is_archived INTEGER DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS app_settings(
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts
        USING fts5(fact, tags, content='memories', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, fact, tags)
            VALUES (new.id, new.fact, COALESCE(new.tags, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, fact, tags)
            VALUES ('delete', old.id, old.fact, COALESCE(old.tags, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, fact, tags)
            VALUES ('delete', old.id, old.fact, COALESCE(old.tags, ''));
            INSERT INTO memories_fts(rowid, fact, tags)
            VALUES (new.id, new.fact, COALESCE(new.tags, ''));
        END;

        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
        USING fts5(content, role, content='messages', content_rowid='rowid');

        CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, content, role)
            VALUES (new.rowid, new.content, new.role);
        END;

        CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, role)
            VALUES ('delete', old.rowid, old.content, old.role);
        END;

        CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, role)
            VALUES ('delete', old.rowid, old.content, old.role);
            INSERT INTO messages_fts(rowid, content, role)
            VALUES (new.rowid, new.content, new.role);
        END;
        "#,
    )
    .map_err(|error| format!("Cannot migrate SQLite schema: {error}"))?;
    ensure_column(conn, "projects", "summary", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "messages", "reasoning", "TEXT")?;
    ensure_fts_schema(conn)?;
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at)
         VALUES('schema_version', ?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![SCHEMA_VERSION, now()],
    )
    .map_err(|error| format!("Cannot set schema version: {error}"))?;
    rebuild_fts(conn)?;
    Ok(())
}

/// Test-only entry point so other modules' tests can build a migrated
/// in-memory database without exposing `migrate` in production code.
#[cfg(test)]
pub(crate) fn migrate_for_tests(conn: &Connection) -> Result<(), String> {
    migrate(conn)
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), String> {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("Cannot inspect table {table}: {error}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("Cannot query columns for {table}: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Cannot collect columns for {table}: {error}"))?;
    if columns.iter().any(|item| item == column) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )
    .map_err(|error| format!("Cannot add column {table}.{column}: {error}"))?;
    Ok(())
}

fn rebuild_fts(conn: &Connection) -> Result<(), String> {
    match try_rebuild_fts(conn) {
        Ok(()) => Ok(()),
        Err(_) => {
            recreate_fts_schema(conn)?;
            try_rebuild_fts(conn)
        }
    }
}

fn try_rebuild_fts(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "INSERT INTO memories_fts(memories_fts) VALUES('rebuild')",
        [],
    )
    .map_err(|error| format!("Cannot rebuild memory FTS index: {error}"))?;
    conn.execute(
        "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
        [],
    )
    .map_err(|error| format!("Cannot rebuild message FTS index: {error}"))?;
    Ok(())
}

fn ensure_fts_schema(conn: &Connection) -> Result<(), String> {
    let fts_version = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'fts_schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Cannot read FTS schema version: {error}"))?;
    let memories_ok = fts_table_has_columns(conn, "memories_fts", &["fact", "tags"])?;
    let messages_ok = fts_table_has_columns(conn, "messages_fts", &["content", "role"])?;
    if fts_version.as_deref() == Some(FTS_SCHEMA_VERSION) && memories_ok && messages_ok {
        return Ok(());
    }
    recreate_fts_schema(conn)
}

fn fts_table_has_columns(
    conn: &Connection,
    table: &str,
    expected: &[&str],
) -> Result<bool, String> {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("Cannot inspect FTS table {table}: {error}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("Cannot query FTS columns for {table}: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Cannot collect FTS columns for {table}: {error}"))?;
    Ok(expected
        .iter()
        .all(|column| columns.iter().any(|item| item == column)))
}

fn recreate_fts_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS memories_ai;
        DROP TRIGGER IF EXISTS memories_ad;
        DROP TRIGGER IF EXISTS memories_au;
        DROP TRIGGER IF EXISTS messages_ai;
        DROP TRIGGER IF EXISTS messages_ad;
        DROP TRIGGER IF EXISTS messages_au;
        DROP TABLE IF EXISTS memories_fts;
        DROP TABLE IF EXISTS messages_fts;

        CREATE VIRTUAL TABLE memories_fts
        USING fts5(fact, tags, content='memories', content_rowid='id', tokenize='trigram');

        CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, fact, tags)
            VALUES (new.id, new.fact, COALESCE(new.tags, ''));
        END;

        CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, fact, tags)
            VALUES ('delete', old.id, old.fact, COALESCE(old.tags, ''));
        END;

        CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, fact, tags)
            VALUES ('delete', old.id, old.fact, COALESCE(old.tags, ''));
            INSERT INTO memories_fts(rowid, fact, tags)
            VALUES (new.id, new.fact, COALESCE(new.tags, ''));
        END;

        CREATE VIRTUAL TABLE messages_fts
        USING fts5(content, role, content='messages', content_rowid='rowid', tokenize='trigram');

        CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, content, role)
            VALUES (new.rowid, new.content, new.role);
        END;

        CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, role)
            VALUES ('delete', old.rowid, old.content, old.role);
        END;

        CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, role)
            VALUES ('delete', old.rowid, old.content, old.role);
            INSERT INTO messages_fts(rowid, content, role)
            VALUES (new.rowid, new.content, new.role);
        END;
        "#,
    )
    .map_err(|error| format!("Cannot recreate FTS schema: {error}"))?;
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at)
         VALUES('fts_schema_version', ?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![FTS_SCHEMA_VERSION, now()],
    )
    .map_err(|error| format!("Cannot set FTS schema version: {error}"))?;
    Ok(())
}

pub fn list_conversations(conn: &Connection) -> Result<Vec<Conversation>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, title, project_id, is_archived, created_at, updated_at
             FROM conversations
             WHERE is_archived = 0
             ORDER BY updated_at DESC",
        )
        .map_err(|error| format!("Cannot prepare conversations query: {error}"))?;
    let rows = statement
        .query_map([], conversation_from_row)
        .map_err(|error| format!("Cannot query conversations: {error}"))?;
    collect_rows(rows)
}

pub fn list_archived_conversations(conn: &Connection) -> Result<Vec<Conversation>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, title, project_id, is_archived, created_at, updated_at
             FROM conversations
             WHERE is_archived = 1
             ORDER BY updated_at DESC",
        )
        .map_err(|error| format!("Cannot prepare archived conversations query: {error}"))?;
    let rows = statement
        .query_map([], conversation_from_row)
        .map_err(|error| format!("Cannot query archived conversations: {error}"))?;
    collect_rows(rows)
}

pub fn create_conversation(
    conn: &Connection,
    title: Option<String>,
    project_id: Option<String>,
) -> Result<Conversation, String> {
    let timestamp = now();
    let conversation = Conversation {
        id: new_id(),
        title: title.unwrap_or_else(|| "新对话".to_string()),
        project_id,
        is_archived: false,
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
    };
    conn.execute(
        "INSERT INTO conversations(id, title, project_id, is_archived, created_at, updated_at)
         VALUES(?1, ?2, ?3, 0, ?4, ?4)",
        params![
            conversation.id,
            conversation.title,
            conversation.project_id,
            conversation.created_at
        ],
    )
    .map_err(|error| format!("Cannot create conversation: {error}"))?;
    Ok(conversation)
}

pub fn get_conversation(conn: &Connection, id: &str) -> Result<Option<Conversation>, String> {
    conn.query_row(
        "SELECT id, title, project_id, is_archived, created_at, updated_at
         FROM conversations WHERE id = ?1",
        params![id],
        conversation_from_row,
    )
    .optional()
    .map_err(|error| format!("Cannot get conversation: {error}"))
}

pub fn archive_conversation(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE conversations SET is_archived = 1, updated_at = ?1 WHERE id = ?2",
        params![now(), id],
    )
    .map_err(|error| format!("Cannot archive conversation: {error}"))?;
    Ok(())
}

pub fn restore_conversation(conn: &Connection, id: &str) -> Result<Conversation, String> {
    conn.execute(
        "UPDATE conversations SET is_archived = 0, updated_at = ?1 WHERE id = ?2",
        params![now(), id],
    )
    .map_err(|error| format!("Cannot restore conversation: {error}"))?;
    get_conversation(conn, id)?.ok_or_else(|| format!("会话不存在: {id}"))
}

pub fn delete_conversation(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])
        .map_err(|error| format!("Cannot delete conversation: {error}"))?;
    Ok(())
}

pub fn move_conversation_to_project(
    conn: &Connection,
    conversation_id: &str,
    project_id: Option<&str>,
) -> Result<Conversation, String> {
    conn.execute(
        "UPDATE conversations SET project_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![project_id, now(), conversation_id],
    )
    .map_err(|error| format!("Cannot move conversation: {error}"))?;
    get_conversation(conn, conversation_id)?.ok_or_else(|| format!("会话不存在: {conversation_id}"))
}

pub fn list_projects(conn: &Connection) -> Result<Vec<Project>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, name, created_at, updated_at, is_archived
             FROM projects
             WHERE is_archived = 0
             ORDER BY updated_at DESC",
        )
        .map_err(|error| format!("Cannot prepare projects query: {error}"))?;
    let rows = statement
        .query_map([], project_from_row)
        .map_err(|error| format!("Cannot query projects: {error}"))?;
    collect_rows(rows)
}

pub fn create_project(conn: &Connection, name: String) -> Result<Project, String> {
    let timestamp = now();
    let project = Project {
        id: new_id(),
        name,
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
        is_archived: false,
    };
    conn.execute(
        "INSERT INTO projects(id, name, created_at, updated_at, is_archived)
         VALUES(?1, ?2, ?3, ?3, 0)",
        params![project.id, project.name, project.created_at],
    )
    .map_err(|error| format!("Cannot create project: {error}"))?;
    Ok(project)
}

pub fn delete_project(conn: &Connection, id: &str) -> Result<(), String> {
    let conversation_ids: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT id FROM conversations WHERE project_id = ?1")
            .map_err(|error| format!("Cannot prepare project conversations: {error}"))?;
        let rows = stmt
            .query_map(params![id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Cannot query project conversations: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("Cannot collect project conversations: {error}"))?
    };
    for cid in &conversation_ids {
        conn.execute(
            "DELETE FROM messages WHERE conversation_id = ?1",
            params![cid],
        )
        .map_err(|error| format!("Cannot delete messages: {error}"))?;
        conn.execute("DELETE FROM conversations WHERE id = ?1", params![cid])
            .map_err(|error| format!("Cannot delete conversation: {error}"))?;
    }
    conn.execute("DELETE FROM projects WHERE id = ?1", params![id])
        .map_err(|error| format!("Cannot delete project: {error}"))?;
    Ok(())
}

pub fn rename_project(conn: &Connection, id: &str, name: &str) -> Result<Project, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Project name cannot be empty".to_string());
    }
    let timestamp = now();
    conn.execute(
        "UPDATE projects SET name = ?1, updated_at = ?2 WHERE id = ?3",
        params![trimmed, timestamp, id],
    )
    .map_err(|error| format!("Cannot rename project: {error}"))?;
    let project = conn
        .query_row(
            "SELECT id, name, created_at, updated_at, is_archived FROM projects WHERE id = ?1",
            params![id],
            project_from_row,
        )
        .map_err(|error| format!("Cannot fetch renamed project: {error}"))?;
    Ok(project)
}

pub fn project_context_messages(
    conn: &Connection,
    project_id: &str,
    current_conversation_id: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<ChatMessage>, String> {
    if let Some(fts_query) = fts_query(query) {
        let mut statement = conn
            .prepare(
                "SELECT m.id, m.conversation_id, m.role, m.content, m.created_at, m.reasoning
             FROM messages_fts f
                 JOIN messages m ON m.rowid = f.rowid
                 JOIN conversations c ON c.id = m.conversation_id
                 WHERE messages_fts MATCH ?1
                   AND c.project_id = ?2
                   AND c.id != ?3
                   AND c.is_archived = 0
                 ORDER BY bm25(messages_fts), m.created_at DESC
                 LIMIT ?4",
            )
            .map_err(|error| format!("Cannot prepare project context FTS query: {error}"))?;
        let rows = statement
            .query_map(
                params![fts_query, project_id, current_conversation_id, limit],
                message_from_row,
            )
            .map_err(|error| format!("Cannot query project context FTS: {error}"))?;
        let mut messages = collect_rows(rows)?;
        if !messages.is_empty() {
            messages.reverse();
            return Ok(messages);
        }
    }

    let keywords = memory_keywords(query);
    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let mut statement = conn
        .prepare(
            "SELECT m.id, m.conversation_id, m.role, m.content, m.created_at, m.reasoning
             FROM messages m
             JOIN conversations c ON c.id = m.conversation_id
             WHERE c.project_id = ?1
               AND c.id != ?2
               AND c.is_archived = 0
             ORDER BY m.created_at DESC
             LIMIT 80",
        )
        .map_err(|error| format!("Cannot prepare project context query: {error}"))?;
    let rows = statement
        .query_map(
            params![project_id, current_conversation_id],
            message_from_row,
        )
        .map_err(|error| format!("Cannot query project context: {error}"))?;
    let mut messages = collect_rows(rows)?
        .into_iter()
        .filter(|message| score_content(&message.content, &keywords) > 0)
        .collect::<Vec<_>>();
    messages.sort_by(|a, b| {
        score_content(&b.content, &keywords)
            .cmp(&score_content(&a.content, &keywords))
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    messages.truncate(limit as usize);
    messages.reverse();
    Ok(messages)
}

pub fn touch_conversation(conn: &Connection, id: &str, title: Option<&str>) -> Result<(), String> {
    let timestamp = now();
    if let Some(title) = title {
        conn.execute(
            "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, timestamp, id],
        )
    } else {
        conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![timestamp, id],
        )
    }
    .map_err(|error| format!("Cannot update conversation: {error}"))?;
    Ok(())
}

pub fn rename_conversation(
    conn: &Connection,
    id: &str,
    title: &str,
) -> Result<Conversation, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("Conversation title cannot be empty".to_string());
    }
    conn.execute(
        "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
        params![trimmed, now(), id],
    )
    .map_err(|error| format!("Cannot rename conversation: {error}"))?;
    conn.query_row(
        "SELECT id, title, project_id, is_archived, created_at, updated_at FROM conversations WHERE id = ?1",
        params![id],
        conversation_from_row,
    )
    .map_err(|error| format!("Cannot fetch renamed conversation: {error}"))
}

pub fn insert_message(
    conn: &Connection,
    conversation_id: &str,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
) -> Result<ChatMessage, String> {
    let message = ChatMessage {
        id: new_id(),
        conversation_id: conversation_id.to_string(),
        role: role.to_string(),
        content: content.to_string(),
        created_at: now(),
        reasoning: reasoning.map(|s| s.to_string()),
    };
    conn.execute(
        "INSERT INTO messages(id, conversation_id, role, content, created_at, reasoning) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            message.id,
            message.conversation_id,
            message.role,
            message.content,
            message.created_at,
            message.reasoning,
        ],
    )
    .map_err(|error| format!("Cannot insert message: {error}"))?;
    Ok(message)
}

pub fn list_messages(conn: &Connection, conversation_id: &str) -> Result<Vec<ChatMessage>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, conversation_id, role, content, created_at, reasoning
             FROM messages WHERE conversation_id = ?1 ORDER BY created_at ASC",
        )
        .map_err(|error| format!("Cannot prepare messages query: {error}"))?;
    let rows = statement
        .query_map(params![conversation_id], message_from_row)
        .map_err(|error| format!("Cannot query messages: {error}"))?;
    collect_rows(rows)
}

pub fn list_model_configs(
    conn: &Connection,
    expose_keys: bool,
) -> Result<Vec<ModelConfig>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, provider, name, base_url, model, api_key, is_default, created_at, updated_at
             FROM model_configs
             ORDER BY is_default DESC, name ASC",
        )
        .map_err(|error| format!("Cannot prepare model config query: {error}"))?;
    let rows = statement
        .query_map([], |row| model_config_from_row(row, expose_keys))
        .map_err(|error| format!("Cannot query model configs: {error}"))?;
    collect_rows(rows)
}

pub fn get_model_config(
    conn: &Connection,
    id: Option<&str>,
    expose_keys: bool,
) -> Result<ModelConfig, String> {
    let sql = if id.is_some() {
        "SELECT id, provider, name, base_url, model, api_key, is_default, created_at, updated_at
         FROM model_configs
         WHERE id = ?1"
    } else {
        "SELECT id, provider, name, base_url, model, api_key, is_default, created_at, updated_at
         FROM model_configs
         ORDER BY is_default DESC, name ASC LIMIT 1"
    };
    let config = if let Some(id) = id {
        conn.query_row(sql, params![id], |row| {
            model_config_from_row(row, expose_keys)
        })
    } else {
        conn.query_row(sql, [], |row| model_config_from_row(row, expose_keys))
    };
    config.map_err(|error| format!("Cannot get model config: {error}"))
}

pub fn get_model_settings(conn: &Connection) -> Result<ModelSettings, String> {
    let fallback_chat_model_id = get_model_config(conn, None, false)
        .ok()
        .map(|config| config.id);
    let chat_model_config_id =
        get_app_setting(conn, "chat_model_config_id")?.or(fallback_chat_model_id);
    let background_model_follows_chat = get_app_setting(conn, "background_model_follows_chat")?
        .map(|value| value != "false")
        .unwrap_or(true);
    let background_model_config_id = get_app_setting(conn, "background_model_config_id")?;
    Ok(ModelSettings {
        chat_model_config_id,
        background_model_config_id,
        background_model_follows_chat,
    })
}

pub fn save_model_settings(
    conn: &Connection,
    settings: ModelSettings,
) -> Result<ModelSettings, String> {
    if let Some(id) = settings.chat_model_config_id.as_deref() {
        get_model_config(conn, Some(id), false)?;
    }
    if let Some(id) = settings.background_model_config_id.as_deref() {
        get_model_config(conn, Some(id), false)?;
    }

    set_app_setting(
        conn,
        "chat_model_config_id",
        settings.chat_model_config_id.as_deref(),
    )?;
    set_app_setting(
        conn,
        "background_model_config_id",
        settings.background_model_config_id.as_deref(),
    )?;
    set_app_setting(
        conn,
        "background_model_follows_chat",
        Some(if settings.background_model_follows_chat {
            "true"
        } else {
            "false"
        }),
    )?;
    get_model_settings(conn)
}

pub fn get_system_prompt(conn: &Connection) -> Result<String, String> {
    Ok(get_app_setting(conn, "system_prompt")?.unwrap_or_default())
}

pub fn save_system_prompt(conn: &Connection, prompt: &str) -> Result<(), String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        set_app_setting(conn, "system_prompt", None)
    } else {
        set_app_setting(conn, "system_prompt", Some(trimmed))
    }
}

fn get_app_setting(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(|error| format!("Cannot read app setting {key}: {error}"))
}

fn set_app_setting(conn: &Connection, key: &str, value: Option<&str>) -> Result<(), String> {
    if let Some(value) = value {
        conn.execute(
            "INSERT INTO app_settings(key, value, updated_at)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now()],
        )
        .map_err(|error| format!("Cannot write app setting {key}: {error}"))?;
    } else {
        conn.execute("DELETE FROM app_settings WHERE key = ?1", params![key])
            .map_err(|error| format!("Cannot clear app setting {key}: {error}"))?;
    }
    Ok(())
}

pub fn save_model_config(conn: &Connection, config: ModelConfig) -> Result<ModelConfig, String> {
    let timestamp = now();
    if config.is_default {
        conn.execute(
            "UPDATE model_configs SET is_default = 0, updated_at = ?1 WHERE id != ?2",
            params![timestamp, config.id],
        )
        .map_err(|error| format!("Cannot clear previous default model config: {error}"))?;
    }
    if let Some(api_key) = config.api_key.as_deref() {
        let trimmed = api_key.trim();
        if trimmed.chars().all(|c| c == '*') {
            // Keep the existing credential.
        } else if trimmed.is_empty() {
            secrets::delete_model_api_key(&config.id)?;
        } else {
            secrets::save_model_api_key(&config.id, trimmed)?;
        }
    }

    conn.execute(
        "INSERT INTO model_configs(id, provider, name, base_url, model, api_key, is_default, created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(id) DO UPDATE SET
           provider = excluded.provider,
           name = excluded.name,
           base_url = excluded.base_url,
           model = excluded.model,
           api_key = NULL,
           is_default = excluded.is_default,
           updated_at = excluded.updated_at",
        params![
            config.id,
            config.provider,
            config.name,
            config.base_url,
            config.model,
            Option::<String>::None,
            bool_to_i64(config.is_default),
            timestamp
        ],
    )
    .map_err(|error| format!("Cannot save model config: {error}"))?;
    get_model_config(conn, Some(&config.id), false)
}

pub fn delete_model_config(conn: &Connection, id: &str) -> Result<(), String> {
    let settings = get_model_settings(conn)?;
    let is_active = settings.chat_model_config_id.as_deref() == Some(id)
        || settings.background_model_config_id.as_deref() == Some(id);
    if is_active {
        return Err("Cannot delete the active chat or background model".to_string());
    }
    conn.execute("DELETE FROM model_configs WHERE id = ?1", params![id])
        .map_err(|error| format!("Cannot delete model config: {error}"))?;
    secrets::delete_model_api_key(id)?;
    Ok(())
}

pub fn list_memories(
    conn: &Connection,
    query: Option<String>,
    tags: Option<Vec<String>>,
    archived: Option<bool>,
) -> Result<Vec<Memory>, String> {
    let mut sql = String::from(
        "SELECT id, fact, memory_type, importance, confidence, tags, source_conversation_id,
                created_at, updated_at, last_used_at, use_count, is_archived
         FROM memories WHERE 1 = 1",
    );
    let archived_value = bool_to_i64(archived.unwrap_or(false));
    sql.push_str(" AND is_archived = ");
    sql.push_str(&archived_value.to_string());

    let query_like = query.filter(|value| !value.trim().is_empty()).map(|value| {
        sql.push_str(" AND (fact LIKE ?1 OR tags LIKE ?1)");
        format!("%{}%", value.trim())
    });
    if tags.as_ref().is_some_and(|items| !items.is_empty()) {
        for tag in tags.unwrap_or_default() {
            sql.push_str(" AND tags LIKE '%");
            sql.push_str(&tag.replace('\'', "''"));
            sql.push_str("%'");
        }
    }
    sql.push_str(" ORDER BY importance DESC, updated_at DESC LIMIT 200");

    let mut statement = conn
        .prepare(&sql)
        .map_err(|error| format!("Cannot prepare memories query: {error}"))?;
    let rows = if let Some(query_like) = query_like {
        statement.query_map(params![query_like], memory_from_row)
    } else {
        statement.query_map([], memory_from_row)
    }
    .map_err(|error| format!("Cannot query memories: {error}"))?;
    collect_rows(rows)
}

pub fn update_memory(conn: &Connection, id: i64, patch: MemoryPatch) -> Result<Memory, String> {
    let current = get_memory(conn, id)?;
    let timestamp = now();
    conn.execute(
        "UPDATE memories
         SET fact = ?1, memory_type = ?2, importance = ?3, confidence = ?4, tags = ?5,
             is_archived = ?6, updated_at = ?7
         WHERE id = ?8",
        params![
            patch.fact.unwrap_or(current.fact),
            patch.memory_type.or(current.memory_type),
            patch.importance.unwrap_or(current.importance),
            patch.confidence.unwrap_or(current.confidence),
            patch.tags.or(current.tags),
            bool_to_i64(patch.is_archived.unwrap_or(current.is_archived)),
            timestamp,
            id
        ],
    )
    .map_err(|error| format!("Cannot update memory: {error}"))?;
    get_memory(conn, id)
}

pub fn get_memory(conn: &Connection, id: i64) -> Result<Memory, String> {
    conn.query_row(
        "SELECT id, fact, memory_type, importance, confidence, tags, source_conversation_id,
                created_at, updated_at, last_used_at, use_count, is_archived
         FROM memories WHERE id = ?1",
        params![id],
        memory_from_row,
    )
    .map_err(|error| format!("Cannot get memory: {error}"))
}

pub fn delete_memory(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM memories WHERE id = ?1", params![id])
        .map_err(|error| format!("Cannot delete memory: {error}"))?;
    Ok(())
}

pub fn insert_memory(
    conn: &Connection,
    fact: &str,
    memory_type: &str,
    importance: i64,
    confidence: f64,
    tags: &str,
    source_conversation_id: &str,
) -> Result<Memory, String> {
    let timestamp = now();
    conn.execute(
        "INSERT INTO memories(fact, memory_type, importance, confidence, tags, source_conversation_id, created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            fact,
            memory_type,
            importance,
            confidence,
            tags,
            source_conversation_id,
            timestamp
        ],
    )
    .map_err(|error| format!("Cannot insert memory: {error}"))?;
    let id = conn.last_insert_rowid();
    get_memory(conn, id)
}

pub fn insert_saved_memory(conn: &Connection, fact: &str) -> Result<Memory, String> {
    insert_memory(conn, fact, "saved", 8, 1.0, r#"["saved","manual"]"#, "")
}

pub fn find_similar_memory(conn: &Connection, fact: &str) -> Result<Option<Memory>, String> {
    let needle: String = fact
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .take(18)
        .collect();
    if needle.chars().count() < 6 {
        return Ok(None);
    }
    let escaped = needle
        .replace("\\", "\\\\")
        .replace("%", "\\%")
        .replace("_", "\\_");
    let like = format!("%{}%", escaped);
    conn.query_row(
        "SELECT id, fact, memory_type, importance, confidence, tags, source_conversation_id,
                created_at, updated_at, last_used_at, use_count, is_archived
         FROM memories
         WHERE is_archived = 0 AND REPLACE(fact, ' ', '') LIKE ?1 ESCAPE '\\'
         ORDER BY importance DESC, updated_at DESC
         LIMIT 1",
        params![like],
        memory_from_row,
    )
    .optional()
    .map_err(|error| format!("Cannot find similar memory: {error}"))
}

pub fn select_memories_for_injection(conn: &Connection, limit: i64) -> Result<Vec<Memory>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, fact, memory_type, importance, confidence, tags, source_conversation_id,
                    created_at, updated_at, last_used_at, use_count, is_archived
             FROM memories
             WHERE is_archived = 0
             ORDER BY importance DESC, COALESCE(last_used_at, updated_at) DESC
             LIMIT ?1",
        )
        .map_err(|error| format!("Cannot prepare memory injection query: {error}"))?;
    let rows = statement
        .query_map(params![limit], memory_from_row)
        .map_err(|error| format!("Cannot query injection memories: {error}"))?;
    collect_rows(rows)
}

pub fn mark_memories_used(conn: &Connection, memories: &[Memory]) -> Result<(), String> {
    let timestamp = now();
    for memory in memories {
        conn.execute(
            "UPDATE memories SET last_used_at = ?1, use_count = use_count + 1 WHERE id = ?2",
            params![timestamp, memory.id],
        )
        .map_err(|error| format!("Cannot mark memory as used: {error}"))?;
    }
    Ok(())
}

pub fn search_memories_for_injection(
    conn: &Connection,
    query: &str,
    limit: i64,
) -> Result<Vec<Memory>, String> {
    if let Some(fts_query) = fts_query(query) {
        let mut statement = conn
            .prepare(
                "SELECT m.id, m.fact, m.memory_type, m.importance, m.confidence, m.tags,
                        m.source_conversation_id, m.created_at, m.updated_at, m.last_used_at,
                        m.use_count, m.is_archived
                 FROM memories_fts f
                 JOIN memories m ON m.id = f.rowid
                 WHERE memories_fts MATCH ?1
                   AND m.is_archived = 0
                 ORDER BY bm25(memories_fts), m.importance DESC, m.updated_at DESC
                 LIMIT ?2",
            )
            .map_err(|error| format!("Cannot prepare memory FTS query: {error}"))?;
        let rows = statement
            .query_map(params![fts_query, limit], memory_from_row)
            .map_err(|error| format!("Cannot query memory FTS: {error}"))?;
        let memories = collect_rows(rows)?;
        if !memories.is_empty() {
            return Ok(memories);
        }
    }

    let keywords = memory_keywords(query);
    if keywords.is_empty() {
        return select_memories_for_injection(conn, limit);
    }

    let mut candidates = list_memories(conn, None, None, Some(false))?;
    candidates.sort_by(|a, b| {
        let b_score = score_memory(b, &keywords);
        let a_score = score_memory(a, &keywords);
        b_score.cmp(&a_score)
    });
    candidates.truncate(limit as usize);
    Ok(candidates)
}

fn conversation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: row.get(0)?,
        title: row.get(1)?,
        project_id: row.get(2)?,
        is_archived: row.get::<_, i64>(3)? == 1,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        is_archived: row.get::<_, i64>(4)? == 1,
    })
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    Ok(ChatMessage {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        created_at: row.get(4)?,
        reasoning: row.get(5)?,
    })
}

fn model_config_from_row(
    row: &rusqlite::Row<'_>,
    expose_keys: bool,
) -> rusqlite::Result<ModelConfig> {
    let id: String = row.get(0)?;
    let (stored_key, credential_status, credential_error) = match secrets::load_model_api_key(&id) {
        Ok(Some(value)) => (Some(value), Some("stored".to_string()), None),
        Ok(None) => (None, Some("missing".to_string()), None),
        Err(error) => (None, Some("error".to_string()), Some(error)),
    };
    Ok(ModelConfig {
        id,
        provider: row.get(1)?,
        name: row.get(2)?,
        base_url: row.get(3)?,
        model: row.get(4)?,
        api_key: if expose_keys {
            stored_key
        } else {
            stored_key.map(|key| "*".repeat(key.len()))
        },
        credential_status,
        credential_error,
        is_default: row.get::<_, i64>(6)? == 1,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn memory_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: row.get(0)?,
        fact: row.get(1)?,
        memory_type: row.get(2)?,
        importance: row.get(3)?,
        confidence: row.get(4)?,
        tags: row.get(5)?,
        source_conversation_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        last_used_at: row.get(9)?,
        use_count: row.get(10)?,
        is_archived: row.get::<_, i64>(11)? == 1,
    })
}

fn memory_keywords(query: &str) -> Vec<String> {
    query_terms(query, 32)
}

fn score_memory(memory: &Memory, keywords: &[String]) -> i64 {
    let mut score = memory.importance * 3 + (memory.confidence * 10.0) as i64;
    for keyword in keywords {
        if memory.fact.contains(keyword) {
            score += 12;
        }
        if memory
            .tags
            .as_ref()
            .is_some_and(|tags| tags.contains(keyword))
        {
            score += 8;
        }
    }
    score + memory.use_count.min(8)
}

fn score_content(content: &str, keywords: &[String]) -> i64 {
    keywords
        .iter()
        .map(|keyword| {
            if content.contains(keyword) {
                1_i64
            } else {
                0_i64
            }
        })
        .sum()
}

fn fts_query(query: &str) -> Option<String> {
    let tokens = query_terms(query, 8)
        .into_iter()
        .filter(|item| item.chars().count() >= 3)
        .map(|item| format!("\"{}\"", item.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return None;
    }
    Some(tokens.join(" OR "))
}

fn query_terms(query: &str, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for raw in query.split(is_query_separator) {
        let cleaned = raw
            .chars()
            .filter(|ch| ch.is_alphanumeric() || is_cjk(*ch))
            .collect::<String>();
        let chars = cleaned.chars().collect::<Vec<_>>();
        if chars.len() >= 2 {
            push_unique_term(&mut terms, &mut seen, cleaned);
        }
        if chars.len() > 4 && chars.iter().any(|ch| is_cjk(*ch)) {
            for size in [4_usize, 2, 3] {
                for window in chars.windows(size) {
                    let term = window.iter().collect::<String>();
                    push_unique_term(&mut terms, &mut seen, term);
                    if terms.len() >= limit {
                        return terms;
                    }
                }
            }
        }
        if terms.len() >= limit {
            break;
        }
    }
    terms.truncate(limit);
    terms
}

fn push_unique_term(terms: &mut Vec<String>, seen: &mut HashSet<String>, term: String) {
    if term.is_empty() || !seen.insert(term.clone()) {
        return;
    }
    terms.push(term);
}

fn is_query_separator(ch: char) -> bool {
    ch.is_whitespace() || ",，。！？；;:：、()（）[]【】\"'`".contains(ch)
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
    )
}

fn collect_rows<T, F>(rows: rusqlite::MappedRows<'_, F>) -> Result<Vec<T>, String>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    rows.collect::<rusqlite::Result<Vec<T>>>()
        .map_err(|error| format!("Cannot collect SQLite rows: {error}"))
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory database should open");
        migrate(&conn).expect("schema should migrate");
        conn
    }

    #[test]
    fn chinese_query_terms_include_searchable_fragments() {
        let terms = query_terms("你还记得我喜欢中文短句吗", 32);
        assert!(terms.iter().any(|term| term == "喜欢中文"));
        assert!(terms.iter().any(|term| term == "中文短句"));
        assert!(terms.iter().any(|term| term == "短句"));
    }

    #[test]
    fn memory_search_matches_chinese_long_query() {
        let conn = memory_test_conn();
        insert_memory(
            &conn,
            "用户偏好：喜欢中文短句和直接结论",
            "chat_history",
            8,
            0.9,
            r#"["chat_history","auto"]"#,
            "",
        )
        .expect("memory should insert");

        let memories = search_memories_for_injection(&conn, "你还记得我喜欢中文短句吗", 3)
            .expect("memory search should run");

        assert_eq!(memories.len(), 1);
        assert!(memories[0].fact.contains("中文短句"));
    }

    fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut statement = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("pragma should prepare");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))
            .expect("pragma should query");
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .expect("columns should collect")
    }

    #[test]
    fn migration_is_idempotent_and_records_schema_versions() {
        let conn = memory_test_conn();
        migrate(&conn).expect("second migration should succeed");

        let schema_version: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version should be recorded");
        assert_eq!(schema_version, SCHEMA_VERSION);

        let fts_version: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'fts_schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("fts schema version should be recorded");
        assert_eq!(fts_version, FTS_SCHEMA_VERSION);

        assert!(table_columns(&conn, "messages").contains(&"reasoning".to_string()));
        let fts_columns = table_columns(&conn, "memories_fts");
        assert!(fts_columns.contains(&"fact".to_string()));
        assert!(fts_columns.contains(&"tags".to_string()));
    }

    #[test]
    fn conversation_lifecycle_round_trips() {
        let conn = memory_test_conn();
        let project =
            create_project(&conn, "Project A".to_string()).expect("project should be created");
        let conversation =
            create_conversation(&conn, Some("First".to_string()), Some(project.id.clone()))
                .expect("conversation should be created");
        assert_eq!(conversation.title, "First");
        assert_eq!(
            conversation.project_id.as_deref(),
            Some(project.id.as_str())
        );

        let user = insert_message(&conn, &conversation.id, "user", "hello", None)
            .expect("user message should insert");
        let assistant = insert_message(
            &conn,
            &conversation.id,
            "assistant",
            "hi there",
            Some("reasoning"),
        )
        .expect("assistant message should insert");

        let messages = list_messages(&conn, &conversation.id).expect("messages should list");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, user.id);
        assert_eq!(messages[1].id, assistant.id);
        assert_eq!(messages[1].reasoning.as_deref(), Some("reasoning"));

        let renamed = rename_conversation(&conn, &conversation.id, "Renamed")
            .expect("conversation should rename");
        assert_eq!(renamed.title, "Renamed");

        archive_conversation(&conn, &conversation.id).expect("conversation should archive");
        assert!(list_conversations(&conn)
            .expect("active list should work")
            .is_empty());
        assert_eq!(
            list_archived_conversations(&conn)
                .expect("archived list should work")
                .len(),
            1
        );

        let restored =
            restore_conversation(&conn, &conversation.id).expect("conversation should restore");
        assert!(!restored.is_archived);

        let moved = move_conversation_to_project(&conn, &conversation.id, None)
            .expect("conversation should move");
        assert!(moved.project_id.is_none());

        delete_conversation(&conn, &conversation.id).expect("conversation should delete");
        assert!(get_conversation(&conn, &conversation.id)
            .expect("conversation should load")
            .is_none());
    }

    #[test]
    fn deleting_a_project_removes_its_conversations_and_messages() {
        let conn = memory_test_conn();
        let project =
            create_project(&conn, "Project B".to_string()).expect("project should be created");
        let conversation = create_conversation(
            &conn,
            Some("In project".to_string()),
            Some(project.id.clone()),
        )
        .expect("conversation should be created");
        insert_message(&conn, &conversation.id, "user", "hello", None)
            .expect("message should insert");

        delete_project(&conn, &project.id).expect("project should delete");

        assert!(list_projects(&conn)
            .expect("projects should list")
            .is_empty());
        assert!(get_conversation(&conn, &conversation.id)
            .expect("conversation should load")
            .is_none());
        assert!(list_messages(&conn, &conversation.id)
            .expect("messages should list")
            .is_empty());
    }

    #[test]
    fn empty_rename_inputs_are_rejected() {
        let conn = memory_test_conn();
        let project =
            create_project(&conn, "Project C".to_string()).expect("project should be created");
        let conversation =
            create_conversation(&conn, None, None).expect("conversation should be created");

        assert!(rename_project(&conn, &project.id, "   ").is_err());
        assert!(rename_conversation(&conn, &conversation.id, "").is_err());
    }

    #[test]
    fn system_prompt_round_trips() {
        let conn = memory_test_conn();
        assert_eq!(get_system_prompt(&conn).expect("prompt should load"), "");

        save_system_prompt(&conn, "  be concise  ").expect("prompt should save");
        assert_eq!(
            get_system_prompt(&conn).expect("prompt should load"),
            "be concise"
        );

        save_system_prompt(&conn, "   ").expect("prompt should clear");
        assert_eq!(get_system_prompt(&conn).expect("prompt should load"), "");

        let settings = get_model_settings(&conn).expect("settings should load");
        assert_eq!(settings.chat_model_config_id, None);
        assert!(settings.background_model_follows_chat);
    }
}
