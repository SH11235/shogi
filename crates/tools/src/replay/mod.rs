//! CSA / PSV / tournament JSONL 共通の棋譜 replay 層（parse・盤面再生・勝敗導出）と、
//! その上の棋譜プレイヤー TUI（`kifu_player` バイナリの実体、`kifu-player` feature）。

pub mod csa_source;
pub mod jsonl_source;
pub mod model;
pub mod psv_source;
// TUI (ratatui/crossterm) は kifu-player 側にだけ必要なので、csa-replay 単独では含めない。
#[cfg(feature = "kifu-player")]
pub mod tui;

pub use csa_source::CsaSource;
pub use jsonl_source::JsonlSource;
pub use model::{
    EvalAccumulator, EvalMetrics, GameIndex, GameIndexEntry, GameOutcomeView, GameRecord,
    GameSource, GameSourceRef, MoveAnnotation, MoveView, PairFileMeta, display_label,
    move_is_legal,
};
pub use psv_source::PsvSource;
