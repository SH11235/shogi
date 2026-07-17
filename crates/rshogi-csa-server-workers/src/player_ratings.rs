//! player ID 単位の Elo / 戦績集計純粋ロジック。

use std::collections::BTreeMap;

use serde::Serialize;

use crate::player_identity::legacy_player_id;

pub const INITIAL_RATING: f64 = 1500.0;
pub const ELO_K_FACTOR: f64 = 32.0;

/// D1 の 1 対局行から集計に必要な値だけを切り出した表現。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerGame {
    pub game_id: String,
    pub ended_at_ms: u64,
    pub black_handle: String,
    pub white_handle: String,
    pub black_player_id: Option<String>,
    pub white_player_id: Option<String>,
    pub result_kind: String,
}

impl PlayerGame {
    pub fn resolved_black_player_id(&self) -> String {
        self.black_player_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| legacy_player_id(&self.black_handle))
    }

    pub fn resolved_white_player_id(&self) -> String {
        self.white_player_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| legacy_player_id(&self.white_handle))
    }
}

/// Players API が返す 1 選手分の集計。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlayerSummary {
    pub player_id: String,
    pub display_name: String,
    pub rating: f64,
    pub wins: u64,
    pub losses: u64,
    pub draws: u64,
    pub games: u64,
    pub last_played_at_ms: u64,
    pub legacy: bool,
}

/// 全履歴を終了時刻昇順（同時刻は game_id 昇順）で標準 Elo 集計する。
///
/// self-play（先後の解決済み ID が同一）は Elo / W-L-D から除外する。結果不確定の
/// `ABORT` も同様に除外するが、選手の存在・表示名・最終対局時刻には反映する。
pub fn aggregate_players(games: &[PlayerGame]) -> Vec<PlayerSummary> {
    let mut ordered: Vec<&PlayerGame> = games.iter().collect();
    ordered
        .sort_by(|a, b| a.ended_at_ms.cmp(&b.ended_at_ms).then_with(|| a.game_id.cmp(&b.game_id)));

    let mut players: BTreeMap<String, PlayerSummary> = BTreeMap::new();
    apply_ordered_games(&mut players, ordered);

    sorted_players(players)
}

/// materialized snapshot の既存 summary に、昇順 cursor page を増分適用する。
pub fn apply_player_games(
    existing: impl IntoIterator<Item = PlayerSummary>,
    games: &[PlayerGame],
) -> Vec<PlayerSummary> {
    let mut players: BTreeMap<String, PlayerSummary> =
        existing.into_iter().map(|player| (player.player_id.clone(), player)).collect();
    let mut ordered: Vec<&PlayerGame> = games.iter().collect();
    ordered
        .sort_by(|a, b| a.ended_at_ms.cmp(&b.ended_at_ms).then_with(|| a.game_id.cmp(&b.game_id)));
    apply_ordered_games(&mut players, ordered);
    sorted_players(players)
}

fn apply_ordered_games<'a>(
    players: &mut BTreeMap<String, PlayerSummary>,
    ordered: impl IntoIterator<Item = &'a PlayerGame>,
) {
    for game in ordered {
        let black_id = game.resolved_black_player_id();
        let white_id = game.resolved_white_player_id();
        observe_player(players, &black_id, &game.black_handle, game.ended_at_ms);
        observe_player(players, &white_id, &game.white_handle, game.ended_at_ms);

        if black_id == white_id {
            continue;
        }
        let (black_score, white_score) = match game.result_kind.as_str() {
            "WIN_BLACK" => (1.0, 0.0),
            "WIN_WHITE" => (0.0, 1.0),
            "DRAW" => (0.5, 0.5),
            _ => continue,
        };
        let black_rating = players[&black_id].rating;
        let white_rating = players[&white_id].rating;
        let black_expected = expected_score(black_rating, white_rating);
        let white_expected = 1.0 - black_expected;

        let black = players.get_mut(&black_id).expect("observed above");
        black.rating += ELO_K_FACTOR * (black_score - black_expected);
        update_record(black, black_score);
        let white = players.get_mut(&white_id).expect("observed above");
        white.rating += ELO_K_FACTOR * (white_score - white_expected);
        update_record(white, white_score);
    }
}

fn sorted_players(players: BTreeMap<String, PlayerSummary>) -> Vec<PlayerSummary> {
    let mut result: Vec<_> = players.into_values().collect();
    result
        .sort_by(|a, b| b.rating.total_cmp(&a.rating).then_with(|| a.player_id.cmp(&b.player_id)));
    result
}

fn observe_player(
    players: &mut BTreeMap<String, PlayerSummary>,
    player_id: &str,
    display_name: &str,
    ended_at_ms: u64,
) {
    let player = players.entry(player_id.to_owned()).or_insert_with(|| PlayerSummary {
        player_id: player_id.to_owned(),
        display_name: display_name.to_owned(),
        rating: INITIAL_RATING,
        wins: 0,
        losses: 0,
        draws: 0,
        games: 0,
        last_played_at_ms: ended_at_ms,
        legacy: player_id.starts_with("legacy_"),
    });
    player.display_name = display_name.to_owned();
    player.last_played_at_ms = ended_at_ms;
}

fn expected_score(rating: f64, opponent_rating: f64) -> f64 {
    1.0 / (1.0 + 10_f64.powf((opponent_rating - rating) / 400.0))
}

fn update_record(player: &mut PlayerSummary, score: f64) {
    if score == 1.0 {
        player.wins += 1;
    } else if score == 0.0 {
        player.losses += 1;
    } else {
        player.draws += 1;
    }
    player.games += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: &str, ended: u64, black: &str, white: &str, result: &str) -> PlayerGame {
        PlayerGame {
            game_id: id.into(),
            ended_at_ms: ended,
            black_handle: black.into(),
            white_handle: white.into(),
            black_player_id: Some(format!("p_{black}")),
            white_player_id: Some(format!("p_{white}")),
            result_kind: result.into(),
        }
    }

    #[test]
    fn equal_ratings_move_sixteen_points_after_decisive_result() {
        let players = aggregate_players(&[game("g1", 1, "alice", "bob", "WIN_BLACK")]);
        let alice = players.iter().find(|p| p.player_id == "p_alice").unwrap();
        let bob = players.iter().find(|p| p.player_id == "p_bob").unwrap();
        assert_eq!((alice.rating, alice.wins, alice.games), (1516.0, 1, 1));
        assert_eq!((bob.rating, bob.losses, bob.games), (1484.0, 1, 1));
    }

    #[test]
    fn chronology_is_ended_at_then_game_id_not_input_order() {
        let games = [
            game("g3", 300, "alice", "bob", "WIN_BLACK"),
            game("g2", 200, "alice", "bob", "WIN_WHITE"),
            game("g1", 100, "alice", "bob", "WIN_BLACK"),
        ];
        let forward = aggregate_players(&games);
        let reverse = aggregate_players(&games.iter().cloned().rev().collect::<Vec<_>>());
        assert_eq!(forward, reverse);
    }

    #[test]
    fn draw_counts_and_preserves_equal_ratings() {
        let players = aggregate_players(&[game("g1", 1, "alice", "bob", "DRAW")]);
        assert!(players.iter().all(|p| p.rating == INITIAL_RATING && p.draws == 1));
    }

    #[test]
    fn self_play_and_abort_are_visible_but_not_rated() {
        let mut self_game = game("g1", 10, "alice", "alice", "WIN_BLACK");
        self_game.white_player_id = self_game.black_player_id.clone();
        let abort = game("g2", 20, "alice", "bob", "ABORT");
        let players = aggregate_players(&[self_game, abort]);
        assert_eq!(players.len(), 2);
        assert!(players.iter().all(|p| p.rating == INITIAL_RATING && p.games == 0));
        assert_eq!(
            players.iter().find(|p| p.player_id == "p_alice").unwrap().last_played_at_ms,
            20
        );
    }

    #[test]
    fn missing_ids_resolve_to_name_derived_legacy_ids() {
        let mut old = game("old", 1, "alice", "bob", "DRAW");
        old.black_player_id = None;
        old.white_player_id = None;
        let players = aggregate_players(&[old]);
        assert!(players.iter().any(|p| p.player_id == legacy_player_id("alice")));
        assert!(players.iter().any(|p| p.player_id == legacy_player_id("bob")));
        assert!(players.iter().all(|p| p.legacy));
    }

    #[test]
    fn summary_wire_fields_match_viewer_contract() {
        let players = aggregate_players(&[game("g1", 123, "alice", "bob", "WIN_BLACK")]);
        let value = serde_json::to_value(&players[0]).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "display_name",
                "draws",
                "games",
                "last_played_at_ms",
                "legacy",
                "losses",
                "player_id",
                "rating",
                "wins",
            ]
        );
    }
}
