use crate::database;
use crate::inference;
use crate::types::{Memory, MemoryPatch, ModelConfig};
use rusqlite::Connection;
use serde::Deserialize;
use std::collections::HashSet;

const MAX_INJECTED_MEMORIES: i64 = 6;
pub(crate) const MEMORY_PLANNER_PROMPT: &str = r#"你是 Mira 的长期记忆 planner。只输出 JSON，不要输出解释。

任务：
1. 判断这一轮对话是否值得写入长期记忆。
2. 只记录稳定、事实性、未来有帮助的信息。
3. 不记录 API Key、密码、token、身份证、银行卡、手机号等敏感信息。
4. 用户明确要求“记住”的内容写入 memory_type="saved"。
5. 非显式要求但有长期价值的用户偏好/背景写入 memory_type="chat_history"。
6. 与当前软件项目稳定相关的事实写入 memory_type="project"。

输出格式：
{"memories":[{"fact":"一句独立事实","memory_type":"saved|chat_history|project","importance":1-10,"confidence":0.0-1.0,"tags":["saved|chat_history|project","auto|explicit"]}]}

如果不值得记录，输出 {"memories":[]}。
"#;

pub fn inject_relevant_memories(
    conn: &Connection,
    user_content: &str,
) -> Result<Vec<Memory>, String> {
    if !should_retrieve_memories(user_content) {
        return Ok(Vec::new());
    }

    let memories =
        database::search_memories_for_injection(conn, user_content, MAX_INJECTED_MEMORIES)?;
    database::mark_memories_used(conn, &memories)?;
    Ok(memories)
}

pub async fn plan_turn_memory_write(
    config: &ModelConfig,
    user_content: &str,
    assistant_content: &str,
) -> MemoryWriteDecision {
    match plan_memory_write_with_llm(config, user_content, assistant_content).await {
        Ok(decision) => decision,
        Err(_) => plan_memory_write(user_content, assistant_content),
    }
}

pub fn observe_turn_with_decision(
    conn: &Connection,
    conversation_id: &str,
    decision: MemoryWriteDecision,
) -> Result<Vec<Memory>, String> {
    apply_memory_decision(conn, conversation_id, decision)
}

fn apply_memory_decision(
    conn: &Connection,
    conversation_id: &str,
    decision: MemoryWriteDecision,
) -> Result<Vec<Memory>, String> {
    match decision {
        MemoryWriteDecision::Noop => Ok(Vec::new()),
        MemoryWriteDecision::Remember(facts) => {
            let mut changed = Vec::new();
            for fact in facts {
                if looks_sensitive(&fact.fact) {
                    continue;
                }
                if let Some(existing) = find_existing_memory_for_candidate(conn, &fact)? {
                    let existing_type = existing.memory_type.as_deref().unwrap_or("");
                    if existing_type == "saved" && fact.memory_type != "saved" {
                        continue;
                    }
                    if should_update_memory(&existing, &fact.fact) {
                        changed.push(database::update_memory(
                            conn,
                            existing.id,
                            MemoryPatch {
                                fact: Some(fact.fact),
                                memory_type: Some(fact.memory_type),
                                importance: Some(fact.importance),
                                confidence: Some(fact.confidence),
                                tags: Some(fact.tags),
                                is_archived: Some(false),
                            },
                        )?);
                    }
                    continue;
                }

                changed.push(database::insert_memory(
                    conn,
                    &fact.fact,
                    &fact.memory_type,
                    fact.importance,
                    fact.confidence,
                    &fact.tags,
                    conversation_id,
                )?);
            }
            Ok(changed)
        }
    }
}

fn find_existing_memory_for_candidate(
    conn: &Connection,
    fact: &CandidateMemory,
) -> Result<Option<Memory>, String> {
    if let Some(existing) = database::find_similar_memory(conn, &fact.fact)? {
        return Ok(Some(existing));
    }
    let memories = database::list_memories(conn, None, None, Some(false))?;
    Ok(memories
        .into_iter()
        .filter(|memory| is_duplicate_fact(&memory.fact, &fact.fact))
        .max_by_key(|memory| {
            let saved_bonus = if memory.memory_type.as_deref() == Some("saved") {
                100
            } else {
                0
            };
            saved_bonus + memory.importance + memory.use_count.min(10)
        }))
}

pub fn run_cleanup(conn: &Connection) -> Result<usize, String> {
    conn.execute(
        "UPDATE memories
         SET importance = MAX(1, importance - 1), updated_at = ?1
         WHERE is_archived = 0
           AND use_count = 0
           AND datetime(created_at) < datetime('now', '-45 days')",
        rusqlite::params![database::now()],
    )
    .map_err(|error| format!("Cannot clean stale memories: {error}"))
}

pub(crate) enum MemoryWriteDecision {
    Remember(Vec<CandidateMemory>),
    Noop,
}

#[derive(Debug, Clone)]
pub(crate) struct CandidateMemory {
    fact: String,
    memory_type: String,
    importance: i64,
    confidence: f64,
    tags: String,
}

#[derive(Debug, Deserialize)]
struct PlannerResponse {
    memories: Vec<PlannerMemory>,
}

#[derive(Debug, Deserialize)]
struct PlannerMemory {
    fact: String,
    memory_type: String,
    importance: Option<i64>,
    confidence: Option<f64>,
    tags: Option<Vec<String>>,
}

fn should_retrieve_memories(content: &str) -> bool {
    let lowered = content.to_lowercase();
    let markers = [
        "继续",
        "上次",
        "之前",
        "还记得",
        "我的",
        "我喜欢",
        "我偏好",
        "记忆",
        "记住",
        "项目",
        "设置",
        "偏好",
        "习惯",
        "remember",
        "preference",
        "previous",
        "last time",
        "my ",
        "project",
    ];
    markers
        .iter()
        .any(|marker| content.contains(marker) || lowered.contains(marker))
}

fn plan_memory_write(user_content: &str, assistant_content: &str) -> MemoryWriteDecision {
    let user_content = user_content.trim();
    if user_content.chars().count() < 6 || looks_sensitive(user_content) {
        return MemoryWriteDecision::Noop;
    }

    let mut facts = Vec::new();
    if let Some(fact) = explicit_memory_fact(user_content) {
        facts.push(CandidateMemory {
            fact,
            memory_type: "saved".to_string(),
            importance: 8,
            confidence: 0.88,
            tags: r#"["saved","explicit"]"#.to_string(),
        });
    } else if let Some(fact) = inferred_preference_fact(user_content) {
        facts.push(CandidateMemory {
            fact,
            memory_type: "chat_history".to_string(),
            importance: 7,
            confidence: 0.78,
            tags: r#"["chat_history","auto"]"#.to_string(),
        });
    } else if let Some(fact) = project_fact(user_content) {
        facts.push(CandidateMemory {
            fact,
            memory_type: "project".to_string(),
            importance: 6,
            confidence: 0.72,
            tags: r#"["project","auto"]"#.to_string(),
        });
    }

    if facts.is_empty() && assistant_confirms_stable_fact(assistant_content) {
        if let Some(fact) = durable_statement_fact(user_content) {
            facts.push(CandidateMemory {
                fact,
                memory_type: "chat_history".to_string(),
                importance: 5,
                confidence: 0.66,
                tags: r#"["chat_history","auto"]"#.to_string(),
            });
        }
    }

    if facts.is_empty() {
        MemoryWriteDecision::Noop
    } else {
        MemoryWriteDecision::Remember(facts)
    }
}

async fn plan_memory_write_with_llm(
    config: &ModelConfig,
    user_content: &str,
    assistant_content: &str,
) -> Result<MemoryWriteDecision, String> {
    let user_block = format!(
        "用户消息：\n{}\n\n助手回复：\n{}",
        compact_for_planner(user_content, 1600),
        compact_for_planner(assistant_content, 1600)
    );
    // The background planner owns an independent session and cancellation token, so a
    // foreground cancel never reaches it. It requests no reasoning, matching the previous
    // non-streaming planner request.
    let content =
        inference::complete_background_text(config, MEMORY_PLANNER_PROMPT, &user_block).await?;
    parse_planner_response(&content)
}

fn parse_planner_response(content: &str) -> Result<MemoryWriteDecision, String> {
    let json = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let response = serde_json::from_str::<PlannerResponse>(json)
        .map_err(|error| format!("Memory planner JSON parse failed: {error}"))?;
    let memories = response
        .memories
        .into_iter()
        .filter_map(normalize_planner_memory)
        .collect::<Vec<_>>();
    if memories.is_empty() {
        Ok(MemoryWriteDecision::Noop)
    } else {
        Ok(MemoryWriteDecision::Remember(memories))
    }
}

fn normalize_planner_memory(memory: PlannerMemory) -> Option<CandidateMemory> {
    let fact = cleanup_fact(&memory.fact);
    if !durable_enough(&fact) || looks_sensitive(&fact) {
        return None;
    }
    let memory_type = match memory.memory_type.as_str() {
        "saved" => "saved",
        "project" => "project",
        _ => "chat_history",
    };
    let tags = memory
        .tags
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| {
            if memory_type == "saved" {
                vec!["saved".to_string(), "explicit".to_string()]
            } else {
                vec![memory_type.to_string(), "auto".to_string()]
            }
        });
    Some(CandidateMemory {
        fact,
        memory_type: memory_type.to_string(),
        importance: memory.importance.unwrap_or(5).clamp(1, 10),
        confidence: memory.confidence.unwrap_or(0.7).clamp(0.0, 1.0),
        tags: serde_json::to_string(&tags)
            .unwrap_or_else(|_| r#"["chat_history","auto"]"#.to_string()),
    })
}

fn explicit_memory_fact(content: &str) -> Option<String> {
    let markers = ["请记住", "帮我记住", "记住", "以后"];
    for marker in markers {
        if let Some(rest) = content.strip_prefix(marker) {
            let fact = cleanup_fact(rest);
            if durable_enough(&fact) {
                return Some(format!("用户明确要求记住：{fact}"));
            }
        }
    }
    None
}

fn inferred_preference_fact(content: &str) -> Option<String> {
    let markers = [
        "我喜欢",
        "我偏好",
        "我习惯",
        "我主要",
        "我不喜欢",
        "我不想",
        "不要",
    ];
    if markers.iter().any(|marker| content.contains(marker)) {
        let fact = cleanup_fact(content);
        if durable_enough(&fact) {
            return Some(format!("用户偏好：{fact}"));
        }
    }

    let lowered = content.to_lowercase();
    let english_markers = ["i prefer", "i like", "i usually", "don't use", "do not use"];
    if english_markers
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return Some(format!("User preference: {}", cleanup_fact(content)));
    }

    None
}

fn project_fact(content: &str) -> Option<String> {
    let markers = [
        "这个项目",
        "当前项目",
        "我们项目",
        "我的项目",
        "项目里",
        "技术栈",
    ];
    if markers.iter().any(|marker| content.contains(marker)) {
        let fact = cleanup_fact(content);
        if durable_enough(&fact) {
            return Some(format!("项目上下文：{fact}"));
        }
    }
    None
}

fn durable_statement_fact(content: &str) -> Option<String> {
    let markers = ["我正在", "我用", "我是", "我的"];
    if markers.iter().any(|marker| content.contains(marker)) {
        let fact = cleanup_fact(content);
        if durable_enough(&fact) {
            return Some(format!("用户提到：{fact}"));
        }
    }
    None
}

fn assistant_confirms_stable_fact(content: &str) -> bool {
    [
        "我会记住",
        "已记住",
        "以后会",
        "我会按",
        "I'll remember",
        "I will remember",
    ]
    .iter()
    .any(|marker| content.contains(marker))
}

fn should_update_memory(existing: &Memory, new_fact: &str) -> bool {
    existing.fact != new_fact && new_fact.chars().count() >= existing.fact.chars().count() / 2
}

fn is_duplicate_fact(existing: &str, candidate: &str) -> bool {
    let existing = normalize_fact_for_match(existing);
    let candidate = normalize_fact_for_match(candidate);
    if existing.chars().count() < 6 || candidate.chars().count() < 6 {
        return false;
    }
    if existing == candidate || existing.contains(&candidate) || candidate.contains(&existing) {
        return true;
    }
    bigram_jaccard(&existing, &candidate) >= 0.62
}

fn normalize_fact_for_match(content: &str) -> String {
    let mut text = content.trim().to_string();
    let prefixes = [
        "用户明确要求记住",
        "用户偏好",
        "用户提到",
        "项目上下文",
        "User preference",
    ];
    loop {
        let before = text.clone();
        for prefix in prefixes {
            text = text
                .trim_start_matches(prefix)
                .trim_start_matches(['：', ':', '，', ',', '。', '.', ' '])
                .to_string();
        }
        if text == before {
            break;
        }
    }
    text.chars()
        .filter(|ch| ch.is_alphanumeric() || *ch as u32 > 127)
        .collect::<String>()
        .to_lowercase()
}

fn bigram_jaccard(left: &str, right: &str) -> f64 {
    let left = bigrams(left);
    let right = bigrams(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count() as f64;
    let union = left.union(&right).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn bigrams(value: &str) -> HashSet<String> {
    let chars = value.chars().collect::<Vec<_>>();
    chars
        .windows(2)
        .map(|window| window.iter().collect::<String>())
        .collect()
}

fn compact_for_planner(content: &str, max_chars: usize) -> String {
    let mut text = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > max_chars {
        text = text.chars().take(max_chars).collect::<String>();
    }
    text
}

fn cleanup_fact(content: &str) -> String {
    content
        .trim()
        .trim_start_matches(['，', ',', '。', '.', ':', '：', ' '])
        .trim_end_matches(['。', '.', '！', '!', ' '])
        .to_string()
}

fn durable_enough(fact: &str) -> bool {
    let len = fact.chars().count();
    (6..=180).contains(&len)
}

fn looks_sensitive(content: &str) -> bool {
    let lowered = content.to_lowercase();
    let sensitive_markers = [
        "api key",
        "apikey",
        "secret",
        "password",
        "密码",
        "密钥",
        "身份证",
        "银行卡",
        "信用卡",
        "手机号",
        "ssn",
        "token",
    ];
    sensitive_markers
        .iter()
        .any(|marker| lowered.contains(marker) || content.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_memory_is_saved() {
        let decision = plan_memory_write("请记住我喜欢简洁的中文回答", "");
        match decision {
            MemoryWriteDecision::Remember(items) => {
                assert_eq!(items[0].memory_type, "saved");
                assert!(items[0].tags.contains("saved"));
            }
            MemoryWriteDecision::Noop => panic!("expected memory write"),
        }
    }

    #[test]
    fn inferred_preference_is_chat_history() {
        let decision = plan_memory_write("我偏好先给结论再解释原因", "");
        match decision {
            MemoryWriteDecision::Remember(items) => {
                assert_eq!(items[0].memory_type, "chat_history");
            }
            MemoryWriteDecision::Noop => panic!("expected memory write"),
        }
    }

    #[test]
    fn planner_json_is_normalized() {
        let decision = parse_planner_response(
            r#"{"memories":[{"fact":"用户偏好短句。","memory_type":"preference","importance":12,"confidence":2,"tags":["preference"]}]}"#,
        )
        .expect("planner JSON should parse");
        match decision {
            MemoryWriteDecision::Remember(items) => {
                assert_eq!(items[0].memory_type, "chat_history");
                assert_eq!(items[0].importance, 10);
                assert_eq!(items[0].confidence, 1.0);
            }
            MemoryWriteDecision::Noop => panic!("expected memory write"),
        }
    }

    #[test]
    fn saved_memory_fact_blocks_chat_history_duplicate() {
        assert!(is_duplicate_fact(
            "喜欢中文短句和直接结论",
            "用户偏好：喜欢中文短句和直接结论"
        ));
        assert!(is_duplicate_fact(
            "用户明确要求记住：回答要先给结论",
            "用户偏好：回答要先给结论"
        ));
    }

    fn memory_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory database should open");
        database::migrate_for_tests(&conn).expect("schema should migrate");
        conn
    }

    #[test]
    fn sensitive_candidates_are_skipped_during_apply() {
        let conn = memory_test_conn();
        let decision = MemoryWriteDecision::Remember(vec![CandidateMemory {
            fact: "my api key is sk-1234567890".to_string(),
            memory_type: "saved".to_string(),
            importance: 8,
            confidence: 0.9,
            tags: r#"["saved"]"#.to_string(),
        }]);

        let changed = apply_memory_decision(&conn, "conversation-1", decision)
            .expect("decision should apply");

        assert!(changed.is_empty());
        assert!(database::list_memories(&conn, None, None, Some(false))
            .expect("memories should list")
            .is_empty());
    }

    #[test]
    fn duplicate_candidates_keep_the_existing_saved_memory() {
        let conn = memory_test_conn();
        database::insert_memory(
            &conn,
            "用户偏好：喜欢中文短句和直接结论",
            "saved",
            8,
            0.9,
            r#"["saved"]"#,
            "",
        )
        .expect("memory should insert");
        let decision = MemoryWriteDecision::Remember(vec![CandidateMemory {
            fact: "喜欢中文短句和直接结论".to_string(),
            memory_type: "chat_history".to_string(),
            importance: 7,
            confidence: 0.8,
            tags: r#"["chat_history","auto"]"#.to_string(),
        }]);

        let changed = apply_memory_decision(&conn, "conversation-1", decision)
            .expect("decision should apply");

        assert!(changed.is_empty());
        let memories =
            database::list_memories(&conn, None, None, Some(false)).expect("memories should list");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type.as_deref(), Some("saved"));
    }

    #[test]
    fn run_cleanup_lowers_importance_of_stale_unused_memories() {
        let conn = memory_test_conn();
        let memory = database::insert_memory(
            &conn,
            "项目使用 Tauri 和 SQLite",
            "project",
            6,
            0.7,
            r#"["project"]"#,
            "",
        )
        .expect("memory should insert");
        conn.execute(
            "UPDATE memories SET created_at = datetime('now', '-90 days') WHERE id = ?1",
            rusqlite::params![memory.id],
        )
        .expect("created_at should update");

        let changed = run_cleanup(&conn).expect("cleanup should run");

        assert_eq!(changed, 1);
        assert_eq!(
            database::get_memory(&conn, memory.id)
                .expect("memory should load")
                .importance,
            5
        );
    }

    fn model_config(api_key: Option<&str>, base_url: &str) -> ModelConfig {
        ModelConfig {
            id: "model-1".to_string(),
            provider: "openai".to_string(),
            name: "Mock provider".to_string(),
            base_url: base_url.to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key: api_key.map(str::to_string),
            credential_status: None,
            credential_error: None,
            is_default: true,
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn planner_without_api_key_falls_back_to_heuristics() {
        let config = model_config(None, "https://api.example.com/v1");

        let decision = plan_turn_memory_write(&config, "请记住我喜欢简洁的中文回答", "").await;

        match decision {
            MemoryWriteDecision::Remember(items) => assert_eq!(items[0].memory_type, "saved"),
            MemoryWriteDecision::Noop => panic!("expected heuristic fallback"),
        }
    }

    #[tokio::test]
    async fn planner_with_an_invalid_base_url_falls_back_to_heuristics() {
        let config = model_config(Some("sk-test"), "not a url");

        let decision = plan_turn_memory_write(&config, "请记住我喜欢简洁的中文回答", "").await;

        match decision {
            MemoryWriteDecision::Remember(items) => assert_eq!(items[0].memory_type, "saved"),
            MemoryWriteDecision::Noop => panic!("expected heuristic fallback"),
        }
    }
}
