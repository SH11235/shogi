//! viewer 検索 D1 migration の additive / 配線契約。

use std::fs;
use std::path::PathBuf;

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn player_id_migration_is_additive_and_declares_both_columns() {
    let sql = repo_file("migrations/0003_games_search_index_player_ids.sql");
    assert!(sql.contains("ADD COLUMN black_player_id TEXT"));
    assert!(sql.contains("ADD COLUMN white_player_id TEXT"));
    let upper = sql.to_ascii_uppercase();
    for forbidden in ["DROP ", "DELETE ", "TRUNCATE ", "RENAME "] {
        assert!(!upper.contains(forbidden), "0003 must be additive; found {forbidden}");
    }
}

#[test]
fn rating_materialization_uses_generation_swap_and_alias_index() {
    let sql = repo_file("migrations/0004_player_rating_materialization.sql");
    for required in [
        "CREATE TABLE player_rating_generations",
        "PRIMARY KEY (generation, player_id)",
        "CREATE TABLE player_rating_state",
        "active_generation INTEGER",
        "rebuild_required INTEGER NOT NULL DEFAULT 1",
        "CREATE TABLE player_id_aliases",
        "ON player_id_aliases (canonical_id, alias_id)",
    ] {
        assert!(sql.contains(required), "rating migration missing {required}");
    }

    let source = repo_file("src/player_rating_materialization.rs");
    assert!(source.contains("WHERE singleton = 1 AND rebuild_required = 0"));
    assert!(source.contains("lease_until_ms <= ?"));

    let search_index = repo_file("src/games_search_index.rs");
    assert!(search_index.contains("NOT EXISTS (SELECT 1 FROM games_search_index"));
    assert!(search_index.contains("db.batch(vec![dirty, upsert])"));

    let api = repo_file("src/viewer_api.rs");
    assert!(api.contains("WHERE generation = ?"));
    assert!(api.contains("COUNT(*) AS total_count FROM games_search_index"));
    assert!(api.contains("ORDER BY ended_at_ms DESC, game_id ASC LIMIT ? OFFSET ?"));
    assert!(api.contains("PLAYER_GAMES_PREDICATE"));
    assert!(api.contains("IN (SELECT alias_id FROM player_id_aliases WHERE canonical_id = ?)"));

    let scheduled = repo_file("src/lib.rs");
    assert!(scheduled.contains("backfill::SCHEDULED_WORK_DEADLINE_MS"));
    assert!(source.contains("deadline_reached(deadline_at_ms"));
    assert!(api.contains("run_player_rating_materialization(env, MAX_PAGES_PER_RUN, None)"));
}

#[test]
fn deploy_workflow_applies_d1_migrations_before_each_worker_deploy() {
    let workflow = repo_file("../../.github/workflows/deploy-workers.yml");
    for env in ["staging", "production"] {
        let migration = format!(
            "wrangler d1 migrations apply GAMES_SEARCH_DB --remote --config wrangler.{env}.toml"
        );
        let deploy = format!("command: deploy --config wrangler.{env}.toml");
        let migration_pos = workflow
            .find(&migration)
            .unwrap_or_else(|| panic!("deploy workflow missing D1 migration command for {env}"));
        let deploy_pos = workflow
            .find(&deploy)
            .unwrap_or_else(|| panic!("deploy workflow missing worker deploy for {env}"));
        assert!(migration_pos < deploy_pos, "{env} migration must run before worker deploy");
    }
}

#[test]
fn secret_sync_validates_player_keyring_without_printing_value() {
    let workflow = repo_file("../../.github/workflows/secret-sync.yml");
    for required in [
        "required_keys=(ADMIN_API_TOKEN PLAYER_ID_SECRET)",
        "utf8bytelength >= 32",
        "length <= 8",
        "try fromjson catch null",
        "^[a-z0-9]{1,16}$",
    ] {
        assert!(workflow.contains(required), "secret strength gate missing {required}");
    }
    assert!(workflow.contains("値は非表示"));
}
