CREATE TABLE games_index_backfill_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    r2_cursor TEXT NOT NULL
);
