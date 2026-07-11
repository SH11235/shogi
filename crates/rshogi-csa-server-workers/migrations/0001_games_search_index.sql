CREATE TABLE games_search_index (
    game_id TEXT PRIMARY KEY,
    sente_name TEXT NOT NULL,
    gote_name TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER NOT NULL,
    result_kind TEXT NOT NULL,
    source TEXT NOT NULL,
    moves_count INTEGER NOT NULL,
    wire_result_kind TEXT NOT NULL,
    end_reason TEXT NOT NULL,
    clock_json TEXT NOT NULL
);

CREATE INDEX games_search_index_ended_at_ms
    ON games_search_index (ended_at_ms DESC);
CREATE TABLE games_search_backfill_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    r2_cursor TEXT NOT NULL
);
