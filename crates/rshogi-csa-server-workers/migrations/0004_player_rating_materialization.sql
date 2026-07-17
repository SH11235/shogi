CREATE TABLE player_rating_generations (
    generation INTEGER NOT NULL,
    player_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    rating REAL NOT NULL,
    wins INTEGER NOT NULL,
    losses INTEGER NOT NULL,
    draws INTEGER NOT NULL,
    games INTEGER NOT NULL,
    last_played_at_ms INTEGER NOT NULL,
    legacy INTEGER NOT NULL CHECK (legacy IN (0, 1)),
    PRIMARY KEY (generation, player_id)
);

CREATE INDEX player_rating_generations_rating
    ON player_rating_generations (generation, rating DESC, player_id ASC);

CREATE TABLE player_rating_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    active_generation INTEGER,
    building_generation INTEGER NOT NULL DEFAULT 0,
    cursor_ended_at_ms INTEGER NOT NULL DEFAULT -1,
    cursor_game_id TEXT NOT NULL DEFAULT '',
    rebuild_required INTEGER NOT NULL DEFAULT 1 CHECK (rebuild_required IN (0, 1)),
    lease_until_ms INTEGER NOT NULL DEFAULT 0
);

INSERT INTO player_rating_state (singleton) VALUES (1);

CREATE TABLE player_id_aliases (
    alias_id TEXT PRIMARY KEY,
    canonical_id TEXT NOT NULL,
    CHECK (alias_id <> canonical_id)
);

CREATE INDEX player_id_aliases_canonical_id
    ON player_id_aliases (canonical_id, alias_id);
