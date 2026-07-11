//! D1 を使った終局済棋譜の検索用二次インデックス。
//!
//! R2 の `games-index/*.json` が正本であり、本モジュールの書き込みはすべて
//! best-effort とする。検索レスポンスを R2 一覧と同じ wire format に戻せるよう、
//! 検索カラムに加えて `end_reason` と `clock_json` も保持する。

use serde::{Deserialize, Serialize};

#[cfg(target_arch = "wasm32")]
use crate::games_index::GamesIndexEntry;

pub const DEFAULT_PAGE_SIZE: u32 = 20;
pub const MAX_PAGE_SIZE: u32 = 100;

pub fn validate_pagination(page: u32, page_size: u32) -> Result<(), String> {
    if page == 0 {
        return Err("page must be a positive integer".into());
    }
    if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(format!("pageSize must be 1..={MAX_PAGE_SIZE}"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchParams {
    pub name: Option<String>,
    pub result: Option<String>,
    pub source: Option<String>,
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryValue {
    Text(String),
    Integer(f64),
}

#[derive(Debug, PartialEq)]
pub struct SearchQuery {
    pub count_sql: String,
    pub rows_sql: String,
    pub filter_values: Vec<QueryValue>,
    pub limit: u32,
    pub offset: u64,
}

/// 検索条件から placeholder 付き SQL と bind 値を組み立てる。
pub fn build_search_query(params: &SearchParams) -> SearchQuery {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    if let Some(name) = &params.name {
        clauses.push("(sente_name LIKE ? ESCAPE '\\' COLLATE NOCASE OR gote_name LIKE ? ESCAPE '\\' COLLATE NOCASE)");
        let escaped = name.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        values.push(QueryValue::Text(pattern.clone()));
        values.push(QueryValue::Text(pattern));
    }
    if let Some(result) = &params.result {
        clauses.push("result_kind = ?");
        values.push(QueryValue::Text(result.clone()));
    }
    if let Some(source) = &params.source {
        clauses.push("source = ?");
        values.push(QueryValue::Text(source.clone()));
    }
    if let Some(from) = params.from {
        clauses.push("ended_at_ms >= ?");
        values.push(QueryValue::Integer(from as f64));
    }
    if let Some(to) = params.to {
        clauses.push("ended_at_ms <= ?");
        values.push(QueryValue::Integer(to as f64));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let columns = "game_id, started_at_ms, ended_at_ms, sente_name, gote_name, wire_result_kind, end_reason, moves_count, clock_json, source";
    SearchQuery {
        count_sql: format!("SELECT COUNT(*) AS total_count FROM games_search_index{where_sql}"),
        rows_sql: format!(
            "SELECT {columns} FROM games_search_index{where_sql} ORDER BY ended_at_ms DESC, game_id ASC LIMIT ? OFFSET ?"
        ),
        filter_values: values,
        limit: params.page_size,
        offset: u64::from(params.page - 1) * u64::from(params.page_size),
    }
}

/// R2 wire の `end_reason` を検索 API の詳細結果分類へ変換する。
pub fn result_kind_for_search(end_reason: &str) -> &'static str {
    match end_reason {
        "RESIGN" => "resignation",
        "TIME_UP" => "time_expired",
        "ILLEGAL" => "abort",
        "JISHOGI" => "jishogi",
        "OUTE_SENNICHITE" => "oute_sennichite",
        "SENNICHITE" => "draw",
        "MAX_MOVES" => "max_moves",
        "ABNORMAL" => "abnormal",
        _ => "abort",
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OwnedGamesIndexEntry {
    pub game_id: String,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub black_handle: String,
    pub white_handle: String,
    pub result_kind: String,
    pub end_reason: String,
    pub moves_count: u32,
    pub clock: serde_json::Value,
    pub source: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchRow {
    game_id: String,
    started_at_ms: u64,
    ended_at_ms: u64,
    sente_name: String,
    gote_name: String,
    wire_result_kind: String,
    end_reason: String,
    moves_count: u32,
    clock_json: String,
    source: String,
}

#[derive(Debug, Serialize)]
pub struct SearchGameSummary {
    game_id: String,
    started_at_ms: u64,
    ended_at_ms: u64,
    black_handle: String,
    white_handle: String,
    result_kind: String,
    end_reason: String,
    moves_count: u32,
    clock: serde_json::Value,
    source: String,
}

impl SearchRow {
    /// R2 の正準 entry を D1 行と同じ表現へ変換する。
    pub fn from_owned(entry: &OwnedGamesIndexEntry) -> Result<Self, serde_json::Error> {
        Ok(Self {
            game_id: entry.game_id.clone(),
            started_at_ms: entry.started_at_ms,
            ended_at_ms: entry.ended_at_ms,
            sente_name: entry.black_handle.clone(),
            gote_name: entry.white_handle.clone(),
            wire_result_kind: entry.result_kind.clone(),
            end_reason: entry.end_reason.clone(),
            moves_count: entry.moves_count,
            clock_json: serde_json::to_string(&entry.clock)?,
            source: entry.source.clone(),
        })
    }

    pub fn into_summary(self) -> Result<SearchGameSummary, serde_json::Error> {
        Ok(SearchGameSummary {
            game_id: self.game_id,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            black_handle: self.sente_name,
            white_handle: self.gote_name,
            result_kind: self.wire_result_kind,
            end_reason: self.end_reason,
            moves_count: self.moves_count,
            clock: serde_json::from_str(&self.clock_json)?,
            source: self.source,
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub async fn upsert_entry(env: &worker::Env, entry: &GamesIndexEntry<'_>) -> worker::Result<()> {
    let clock_json = serde_json::to_string(&entry.clock)?;
    upsert_fields(
        env,
        UpsertFields {
            game_id: entry.game_id,
            sente: entry.black_handle,
            gote: entry.white_handle,
            started_at_ms: entry.started_at_ms,
            ended_at_ms: entry.ended_at_ms,
            wire_result_kind: entry.result_kind,
            end_reason: entry.end_reason,
            moves_count: entry.moves_count,
            clock_json: &clock_json,
            source: entry.source,
        },
    )
    .await
}

#[cfg(target_arch = "wasm32")]
pub async fn upsert_owned(env: &worker::Env, entry: &OwnedGamesIndexEntry) -> worker::Result<()> {
    let clock_json = serde_json::to_string(&entry.clock)?;
    upsert_fields(
        env,
        UpsertFields {
            game_id: &entry.game_id,
            sente: &entry.black_handle,
            gote: &entry.white_handle,
            started_at_ms: entry.started_at_ms,
            ended_at_ms: entry.ended_at_ms,
            wire_result_kind: &entry.result_kind,
            end_reason: &entry.end_reason,
            moves_count: entry.moves_count,
            clock_json: &clock_json,
            source: &entry.source,
        },
    )
    .await
}

#[cfg(target_arch = "wasm32")]
struct UpsertFields<'a> {
    game_id: &'a str,
    sente: &'a str,
    gote: &'a str,
    started_at_ms: u64,
    ended_at_ms: u64,
    wire_result_kind: &'a str,
    end_reason: &'a str,
    moves_count: u32,
    clock_json: &'a str,
    source: &'a str,
}

#[cfg(target_arch = "wasm32")]
async fn upsert_fields(env: &worker::Env, fields: UpsertFields<'_>) -> worker::Result<()> {
    use worker::wasm_bindgen::JsValue;
    let db = env.d1(crate::config::ConfigKeys::GAMES_SEARCH_DB_BINDING)?;
    let values = [
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.sente),
        JsValue::from_str(fields.gote),
        JsValue::from_f64(fields.started_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(result_kind_for_search(fields.end_reason)),
        JsValue::from_str(fields.source),
        JsValue::from_f64(f64::from(fields.moves_count)),
        JsValue::from_str(fields.wire_result_kind),
        JsValue::from_str(fields.end_reason),
        JsValue::from_str(fields.clock_json),
    ];
    db.prepare("INSERT INTO games_search_index (game_id, sente_name, gote_name, started_at_ms, ended_at_ms, result_kind, source, moves_count, wire_result_kind, end_reason, clock_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(game_id) DO UPDATE SET sente_name=excluded.sente_name, gote_name=excluded.gote_name, started_at_ms=excluded.started_at_ms, ended_at_ms=excluded.ended_at_ms, result_kind=excluded.result_kind, source=excluded.source, moves_count=excluded.moves_count, wire_result_kind=excluded.wire_result_kind, end_reason=excluded.end_reason, clock_json=excluded.clock_json")
        .bind(&values)?.run().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base_params() -> SearchParams {
        SearchParams {
            name: None,
            result: None,
            source: None,
            from: None,
            to: None,
            page: 1,
            page_size: 20,
        }
    }
    #[test]
    fn query_supports_all_filters() {
        let query = build_search_query(&SearchParams {
            name: Some("藤井".into()),
            result: Some("resignation".into()),
            source: Some("kifu".into()),
            from: Some(100),
            to: Some(200),
            ..base_params()
        });
        for clause in [
            "sente_name LIKE ? ESCAPE '\\' COLLATE NOCASE",
            "result_kind = ?",
            "source = ?",
            "ended_at_ms >= ?",
            "ended_at_ms <= ?",
        ] {
            assert!(query.count_sql.contains(clause));
        }
        assert_eq!(query.filter_values[0], QueryValue::Text("%藤井%".into()));
    }

    #[test]
    fn name_filter_escapes_like_wildcards_as_literals() {
        let query = build_search_query(&SearchParams {
            name: Some(r"a%b_c\d".into()),
            ..base_params()
        });
        assert_eq!(
            query.filter_values,
            vec![
                QueryValue::Text(r"%a\%b\_c\\d%".into()),
                QueryValue::Text(r"%a\%b\_c\\d%".into()),
            ]
        );
        assert!(query.rows_sql.contains("LIKE ? ESCAPE '\\' COLLATE NOCASE"));
    }
    #[test]
    fn query_calculates_one_based_page_offset() {
        let query = build_search_query(&SearchParams {
            page: 3,
            page_size: 25,
            ..base_params()
        });
        assert_eq!((query.limit, query.offset), (25, 50));
    }
    #[test]
    fn result_kind_matches_canonical_mapping_for_all_end_reasons() {
        for (end_reason, expected) in [
            ("RESIGN", "resignation"),
            ("TIME_UP", "time_expired"),
            ("ILLEGAL", "abort"),
            ("JISHOGI", "jishogi"),
            ("OUTE_SENNICHITE", "oute_sennichite"),
            ("SENNICHITE", "draw"),
            ("MAX_MOVES", "max_moves"),
            ("ABNORMAL", "abnormal"),
        ] {
            assert_eq!(result_kind_for_search(end_reason), expected, "{end_reason}");
        }
        assert_eq!(result_kind_for_search("UNKNOWN"), "abort");
    }

    #[test]
    fn search_summary_preserves_games_index_wire_shape() {
        let original = serde_json::json!({
            "game_id": "g-1", "started_at_ms": 100, "ended_at_ms": 200,
            "black_handle": "alice", "white_handle": "bob",
            "result_kind": "WIN_BLACK", "end_reason": "RESIGN", "moves_count": 42,
            "clock": {"kind": "fischer", "total_sec": 300, "increment_sec": 5},
            "source": "kifu"
        });
        let entry: OwnedGamesIndexEntry = serde_json::from_value(original.clone()).unwrap();
        let summary = SearchRow::from_owned(&entry).unwrap().into_summary().unwrap();
        assert_eq!(serde_json::to_value(summary).unwrap(), original);
    }

    #[test]
    fn pagination_rejects_zero_and_over_maximum() {
        assert!(validate_pagination(0, 20).is_err());
        assert!(validate_pagination(1, 0).is_err());
        assert!(validate_pagination(1, 101).is_err());
        assert!(validate_pagination(1, 100).is_ok());
    }
}
