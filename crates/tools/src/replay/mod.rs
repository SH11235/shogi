//! PSV / tournament JSONL 共通の棋譜プレイヤー（`kifu_player` バイナリの実体）。

pub mod csa_source;
pub mod jsonl_source;
pub mod model;
pub mod psv_source;
pub mod tui;

pub use csa_source::CsaSource;
pub use jsonl_source::JsonlSource;
pub use model::{
    EvalAccumulator, EvalMetrics, GameIndex, GameIndexEntry, GameOutcomeView, GameRecord,
    GameSource, GameSourceRef, MoveAnnotation, MoveView, PairFileMeta, display_label,
    move_is_legal,
};
pub use psv_source::PsvSource;
