//! D1 を使った終局済棋譜の検索用二次インデックス。
//!
//! R2 の `games-index/*.json` が正本であり、本モジュールの書き込みはすべて
//! best-effort とする。検索レスポンスを R2 一覧と同じ wire format に戻せるよう、
//! 検索カラムに加えて `end_reason` と `clock_json` も保持する。

use serde::{Deserialize, Serialize};

#[cfg(any(target_arch = "wasm32", test))]
use std::collections::{BTreeMap, BTreeSet};

#[cfg(target_arch = "wasm32")]
use crate::games_index::GamesIndexEntry;

pub const DEFAULT_PAGE_SIZE: u32 = 20;
pub const MAX_PAGE_SIZE: u32 = 100;

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AliasLink {
    alias_id: String,
    canonical_id: String,
}

/// A new canonical ID absorbs both the freshly derived aliases and every
/// historical alias whose current target is one of those IDs. The canonical
/// ID itself is never retained as an alias, preventing cycles.
#[cfg(any(target_arch = "wasm32", test))]
fn normalized_alias_links(
    existing: &[AliasLink],
    canonical_id: &str,
    aliases: &[String],
) -> BTreeMap<String, String> {
    let targets: BTreeSet<&str> = std::iter::once(canonical_id)
        .chain(aliases.iter().map(String::as_str))
        .collect();
    let mut normalized = BTreeMap::new();
    for link in existing {
        if link.alias_id == canonical_id {
            continue;
        }
        let target = if targets.contains(link.canonical_id.as_str()) {
            canonical_id.to_owned()
        } else {
            link.canonical_id.clone()
        };
        normalized.insert(link.alias_id.clone(), target);
    }
    for alias in aliases {
        if alias != canonical_id {
            normalized.insert(alias.clone(), canonical_id.to_owned());
        }
    }
    normalized
}

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
    let columns = "game_id, started_at_ms, ended_at_ms, sente_name, gote_name, black_player_id, white_player_id, wire_result_kind, end_reason, moves_count, clock_json, source";
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
    #[serde(default)]
    pub black_player_id: Option<String>,
    #[serde(default)]
    pub white_player_id: Option<String>,
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
    black_player_id: Option<String>,
    white_player_id: Option<String>,
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
    black_player_id: String,
    white_player_id: String,
    result_kind: String,
    end_reason: String,
    moves_count: u32,
    clock: serde_json::Value,
    source: String,
}

impl SearchRow {
    pub fn to_player_game(&self) -> crate::player_ratings::PlayerGame {
        crate::player_ratings::PlayerGame {
            game_id: self.game_id.clone(),
            ended_at_ms: self.ended_at_ms,
            black_handle: self.sente_name.clone(),
            white_handle: self.gote_name.clone(),
            black_player_id: self.black_player_id.clone(),
            white_player_id: self.white_player_id.clone(),
            result_kind: self.wire_result_kind.clone(),
        }
    }

    pub fn resolved_player_ids(&self) -> (String, String) {
        let black = self
            .black_player_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| crate::player_identity::legacy_player_id(&self.sente_name));
        let white = self
            .white_player_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| crate::player_identity::legacy_player_id(&self.gote_name));
        (black, white)
    }

    /// R2 の正準 entry を D1 行と同じ表現へ変換する。
    pub fn from_owned(entry: &OwnedGamesIndexEntry) -> Result<Self, serde_json::Error> {
        Ok(Self {
            game_id: entry.game_id.clone(),
            started_at_ms: entry.started_at_ms,
            ended_at_ms: entry.ended_at_ms,
            sente_name: entry.black_handle.clone(),
            gote_name: entry.white_handle.clone(),
            black_player_id: entry.black_player_id.clone(),
            white_player_id: entry.white_player_id.clone(),
            wire_result_kind: entry.result_kind.clone(),
            end_reason: entry.end_reason.clone(),
            moves_count: entry.moves_count,
            clock_json: serde_json::to_string(&entry.clock)?,
            source: entry.source.clone(),
        })
    }

    pub fn into_summary(self) -> Result<SearchGameSummary, serde_json::Error> {
        let (black_player_id, white_player_id) = self.resolved_player_ids();
        Ok(SearchGameSummary {
            game_id: self.game_id,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            black_handle: self.sente_name,
            white_handle: self.gote_name,
            black_player_id,
            white_player_id,
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
            black_player_id: entry.black_player_id,
            white_player_id: entry.white_player_id,
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
            black_player_id: entry.black_player_id.as_deref(),
            white_player_id: entry.white_player_id.as_deref(),
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
    black_player_id: Option<&'a str>,
    white_player_id: Option<&'a str>,
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
    let legacy_black;
    let black_player_id = match fields.black_player_id {
        Some(id) => id,
        None => {
            legacy_black = crate::player_identity::legacy_player_id(fields.sente);
            &legacy_black
        }
    };
    let legacy_white;
    let white_player_id = match fields.white_player_id {
        Some(id) => id,
        None => {
            legacy_white = crate::player_identity::legacy_player_id(fields.gote);
            &legacy_white
        }
    };
    let values = [
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.sente),
        JsValue::from_str(fields.gote),
        JsValue::from_str(black_player_id),
        JsValue::from_str(white_player_id),
        JsValue::from_f64(fields.started_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(result_kind_for_search(fields.end_reason)),
        JsValue::from_str(fields.source),
        JsValue::from_f64(f64::from(fields.moves_count)),
        JsValue::from_str(fields.wire_result_kind),
        JsValue::from_str(fields.end_reason),
        JsValue::from_str(fields.clock_json),
    ];
    let dirty_values = vec![
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.sente),
        JsValue::from_str(fields.gote),
        JsValue::from_str(black_player_id),
        JsValue::from_str(white_player_id),
        JsValue::from_f64(fields.started_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(fields.wire_result_kind),
        JsValue::from_str(fields.end_reason),
        JsValue::from_str(fields.source),
        JsValue::from_f64(f64::from(fields.moves_count)),
        JsValue::from_str(fields.clock_json),
    ];
    // cursor 以前への late insert/update は Elo の後続順序を変える。変更判定を
    // UPSERT 前に行い、dirty marker と UPSERT を同じ D1 transaction に入れる。
    // これにより marker だけ失敗して同一 backfill retry が no-op になる穴を防ぐ。
    let dirty = db
        .prepare("UPDATE player_rating_state SET rebuild_required = 1 WHERE singleton = 1 AND (cursor_ended_at_ms > ? OR (cursor_ended_at_ms = ? AND cursor_game_id >= ?)) AND (NOT EXISTS (SELECT 1 FROM games_search_index WHERE game_id = ?) OR EXISTS (SELECT 1 FROM games_search_index WHERE game_id = ? AND (sente_name IS NOT ? OR gote_name IS NOT ? OR black_player_id IS NOT ? OR white_player_id IS NOT ? OR started_at_ms IS NOT ? OR ended_at_ms IS NOT ? OR wire_result_kind IS NOT ? OR end_reason IS NOT ? OR source IS NOT ? OR moves_count IS NOT ? OR clock_json IS NOT ?)))")
        .bind(&dirty_values)?;
    let upsert = db.prepare("INSERT INTO games_search_index (game_id, sente_name, gote_name, black_player_id, white_player_id, started_at_ms, ended_at_ms, result_kind, source, moves_count, wire_result_kind, end_reason, clock_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(game_id) DO UPDATE SET sente_name=excluded.sente_name, gote_name=excluded.gote_name, black_player_id=excluded.black_player_id, white_player_id=excluded.white_player_id, started_at_ms=excluded.started_at_ms, ended_at_ms=excluded.ended_at_ms, result_kind=excluded.result_kind, source=excluded.source, moves_count=excluded.moves_count, wire_result_kind=excluded.wire_result_kind, end_reason=excluded.end_reason, clock_json=excluded.clock_json WHERE sente_name IS NOT excluded.sente_name OR gote_name IS NOT excluded.gote_name OR black_player_id IS NOT excluded.black_player_id OR white_player_id IS NOT excluded.white_player_id OR ended_at_ms IS NOT excluded.ended_at_ms OR wire_result_kind IS NOT excluded.wire_result_kind OR end_reason IS NOT excluded.end_reason OR started_at_ms IS NOT excluded.started_at_ms OR source IS NOT excluded.source OR moves_count IS NOT excluded.moves_count OR clock_json IS NOT excluded.clock_json")
        .bind(&values)?;
    db.batch(vec![dirty, upsert]).await?;
    Ok(())
}

/// keyring rotation で導出した旧 ID → active canonical ID alias を best-effort 登録する。
#[cfg(target_arch = "wasm32")]
pub async fn register_player_aliases(
    env: &worker::Env,
    canonical_id: &str,
    aliases: &[String],
) -> worker::Result<()> {
    use worker::wasm_bindgen::JsValue;

    if aliases.len() > crate::player_identity::MAX_KEYRING_KEYS {
        return Err(worker::Error::RustError("player alias limit exceeded".into()));
    }
    let db = env.d1(crate::config::ConfigKeys::GAMES_SEARCH_DB_BINDING)?;
    let targets: BTreeSet<_> = std::iter::once(canonical_id)
        .chain(aliases.iter().map(String::as_str))
        .collect();
    let placeholders = std::iter::repeat_n("?", targets.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT alias_id, canonical_id FROM player_id_aliases WHERE alias_id IN ({placeholders}) OR canonical_id IN ({placeholders})"
    );
    let mut lookup_values: Vec<_> = targets.iter().map(|id| JsValue::from_str(id)).collect();
    lookup_values.extend(targets.iter().map(|id| JsValue::from_str(id)));
    #[derive(serde::Deserialize)]
    struct ExistingAliasRow {
        alias_id: String,
        canonical_id: String,
    }
    let existing: Vec<AliasLink> = db
        .prepare(&sql)
        .bind(&lookup_values)?
        .all()
        .await?
        .results::<ExistingAliasRow>()?
        .into_iter()
        .map(|row| AliasLink {
            alias_id: row.alias_id,
            canonical_id: row.canonical_id,
        })
        .collect();
    let current: BTreeMap<_, _> = existing
        .iter()
        .map(|link| (link.alias_id.clone(), link.canonical_id.clone()))
        .collect();
    let normalized = normalized_alias_links(&existing, canonical_id, aliases);
    if current == normalized {
        return Ok(());
    }

    let mut statements = Vec::new();
    let mut reparent_values = vec![JsValue::from_str(canonical_id)];
    reparent_values.extend(targets.iter().map(|id| JsValue::from_str(id)));
    reparent_values.push(JsValue::from_str(canonical_id));
    statements.push(
        db.prepare(&format!(
            "UPDATE player_id_aliases SET canonical_id = ? WHERE canonical_id IN ({placeholders}) AND alias_id <> ?"
        ))
        .bind(&reparent_values)?,
    );
    let delete_values = [JsValue::from_str(canonical_id)];
    statements.push(
        db.prepare("DELETE FROM player_id_aliases WHERE alias_id = ?")
            .bind(&delete_values)?,
    );
    for alias in aliases {
        if alias == canonical_id {
            continue;
        }
        let values = [JsValue::from_str(alias), JsValue::from_str(canonical_id)];
        statements.push(
            db.prepare("INSERT INTO player_id_aliases (alias_id, canonical_id) VALUES (?, ?) ON CONFLICT(alias_id) DO UPDATE SET canonical_id=excluded.canonical_id")
                .bind(&values)?,
        );
    }
    // Reparenting, cycle removal, fresh aliases and rebuild marker commit
    // atomically. A transient failure cannot leave a half-rotated alias graph.
    statements.push(
        db.prepare("UPDATE player_rating_state SET rebuild_required = 1 WHERE singleton = 1"),
    );
    db.batch(statements).await?;
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
        let mut expected = original;
        expected["black_player_id"] =
            serde_json::Value::String(crate::player_identity::legacy_player_id("alice"));
        expected["white_player_id"] =
            serde_json::Value::String(crate::player_identity::legacy_player_id("bob"));
        assert_eq!(serde_json::to_value(summary).unwrap(), expected);
    }

    #[test]
    fn pagination_rejects_zero_and_over_maximum() {
        assert!(validate_pagination(0, 20).is_err());
        assert!(validate_pagination(1, 0).is_err());
        assert!(validate_pagination(1, 101).is_err());
        assert!(validate_pagination(1, 100).is_ok());
    }

    #[test]
    fn second_rotation_reparents_plain_v1_and_v2_urls_to_v3_without_v1_key() {
        const V1: &str = "0123456789abcdef0123456789abcdef";
        const V2: &str = "abcdef0123456789abcdef0123456789";
        const V3: &str = "fedcba9876543210fedcba9876543210";
        let plain_v1 =
            crate::player_identity::derive_player_identity("alice", "pw", Some(V1)).unwrap();
        let v2_ring = format!(r#"{{"active_version":"v2","keys":{{"v1":"{V1}","v2":"{V2}"}}}}"#);
        let v2 =
            crate::player_identity::derive_player_identity("alice", "pw", Some(&v2_ring)).unwrap();
        let after_v2 = normalized_alias_links(&[], &v2.player_id, &v2.aliases);
        assert_eq!(after_v2.get(&plain_v1.player_id), Some(&v2.player_id));

        // v1 has deliberately been removed from the second rotated keyring.
        let v3_ring = format!(r#"{{"active_version":"v3","keys":{{"v2":"{V2}","v3":"{V3}"}}}}"#);
        let v3 =
            crate::player_identity::derive_player_identity("alice", "pw", Some(&v3_ring)).unwrap();
        let existing: Vec<_> = after_v2
            .into_iter()
            .map(|(alias_id, canonical_id)| AliasLink {
                alias_id,
                canonical_id,
            })
            .collect();
        let after_v3 = normalized_alias_links(&existing, &v3.player_id, &v3.aliases);

        // Old detail URLs and games carrying the v2 ID now resolve directly to
        // v3; the materializer therefore rebuilds all ratings under v3 too.
        assert_eq!(after_v3.get(&plain_v1.player_id), Some(&v3.player_id));
        assert_eq!(after_v3.get(&v2.player_id), Some(&v3.player_id));
        assert!(!after_v3.contains_key(&v3.player_id));
        assert!(after_v3.values().all(|target| target == &v3.player_id));
    }
}
