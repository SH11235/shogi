ALTER TABLE games_search_index ADD COLUMN black_player_id TEXT;
ALTER TABLE games_search_index ADD COLUMN white_player_id TEXT;

CREATE INDEX games_search_index_black_player_id_ended_at_ms
    ON games_search_index (black_player_id, ended_at_ms DESC);
CREATE INDEX games_search_index_white_player_id_ended_at_ms
    ON games_search_index (white_player_id, ended_at_ms DESC);
