//! 棋譜プレイヤー TUI が PSV / tournament JSONL を共通に扱うためのデータモデル。

use std::path::PathBuf;

use anyhow::Result;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};

/// 索引フェーズで構築する、対局1件分のメタ情報（局面・指し手は含まない）。
///
/// `build_index` はこれを対局数ぶんだけ保持する。手の内容は `load_game` を
/// 呼ぶまで読み込まないため、ピークメモリは総対局数に比例し総手数には依存しない。
#[derive(Debug, Clone)]
pub struct GameIndexEntry {
    pub source: GameSourceRef,
    /// 対局の最終結果から導出済みの値（生のスコア符号は保持しない）。
    pub outcome: Option<GameOutcomeView>,
    /// JSONL の `result.error` を伝播。PSV は常に false。
    pub error: bool,
    pub ply_count: u32,
    /// 検索 UI 用（JSONL のみ）。再現性ある対局指定に使う。
    pub pair_index: Option<u32>,
    pub pair_slot: Option<u32>,
    pub startpos_idx: Option<u32>,
    /// 索引時に収穫する評価値ベースの派生指標（すべて**先手視点** cp）。ソート・検索用で、
    /// 評価値が 1 つも無い対局は `None`。手番相対 score を持つ PSV/JSONL は先手視点へ変換
    /// してから、元々先手視点の CSA はそのまま `EvalAccumulator` に流す。
    pub metrics: EvalMetrics,
}

/// 対局の評価値の要約（先手視点 cp）。`EvalAccumulator` で 1 パス収穫する。ソート・検索の
/// 目安用のヒューリスティックで、詰みの扱いは出典依存（JSONL は `score_mate` を無視、PSV/CSA
/// は生の score/コメント値をそのまま算入）。ビューアは一度に 1 出典しか開かないため出典間の
/// 厳密比較は前提にしない。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvalMetrics {
    /// 最後の評価値付きの手の cp（決着の大きさ = `|final_cp|`）。
    pub final_cp: Option<i32>,
    pub min_cp: Option<i32>,
    pub max_cp: Option<i32>,
    /// 連続する評価値付きの手の間の `|Δcp|` の最大（評価値の振れ幅）。
    pub max_swing_cp: Option<u32>,
}

impl EvalMetrics {
    /// 「形勢逆転」判定：両者が `threshold` cp 以上優勢になった局面があるか
    /// （`min_cp ≤ -threshold` かつ `max_cp ≥ +threshold`）。
    pub fn had_reversal(&self, threshold: i32) -> bool {
        matches!((self.min_cp, self.max_cp), (Some(lo), Some(hi)) if lo <= -threshold && hi >= threshold)
    }
}

/// 先手視点 cp を手順で流し込んで `EvalMetrics` を 1 パスで組み立てる。
#[derive(Default)]
pub struct EvalAccumulator {
    metrics: EvalMetrics,
    prev_cp: Option<i32>,
}

impl EvalAccumulator {
    /// 先手視点 cp を 1 手ぶん流す（評価値の無い手は呼ばない）。
    pub fn push(&mut self, black_pov_cp: i32) {
        self.metrics.final_cp = Some(black_pov_cp);
        self.metrics.min_cp =
            Some(self.metrics.min_cp.map_or(black_pov_cp, |m| m.min(black_pov_cp)));
        self.metrics.max_cp =
            Some(self.metrics.max_cp.map_or(black_pov_cp, |m| m.max(black_pov_cp)));
        if let Some(prev) = self.prev_cp {
            // 外部棋譜の異常値でも panic しないよう差分は i64 で取り u32 に飽和させる
            // （`i32::MAX - i32::MIN` は i32 に収まらない）。
            let swing =
                i64::from(black_pov_cp).abs_diff(i64::from(prev)).min(u64::from(u32::MAX)) as u32;
            self.metrics.max_swing_cp =
                Some(self.metrics.max_swing_cp.map_or(swing, |s| s.max(swing)));
        }
        self.prev_cp = Some(black_pov_cp);
    }

    /// 収穫した `EvalMetrics` を取り出す（`push` で流し込んだ結果を確定する）。
    pub fn finish(self) -> EvalMetrics {
        self.metrics
    }
}

/// `pos` の合法手集合に `mv` が含まれるか。合法手生成は `pos` からのみ手を作るので、CSA/PSV
/// 由来の非合法手（空マス発・成り不正）を渡しても panic せず判定できる。`Move::raw()` は下位
/// 16bit（from/to/成り/打ち）で、`from_usi`/`move16_to_move` が埋めない上位ビット（移動後の駒）
/// を含まないため、`==` ではなく `raw()` 一致で意味比較する。
pub fn move_is_legal(pos: &Position, mv: Move) -> bool {
    let mut list = MoveList::new();
    generate_legal_all(pos, &mut list);
    list.iter().any(|&g| g.raw() == mv.raw())
}

/// 対局の出典と、その対局を再生するために必要な位置情報。
#[derive(Debug, Clone, Copy)]
pub enum GameSourceRef {
    /// PSV ストリーム中の `[start_record, end_record)` レコード範囲。
    /// `ordinal` は表示用の通し番号（0-indexed）。
    Psv {
        start_record: u64,
        end_record: u64,
        ordinal: u32,
    },
    /// out-dir 横断インデックスの中での位置。
    /// `start_offset`/`end_offset` はペアファイル内のバイト範囲 `[start, end)`
    /// （`meta` 行を含まず、対象対局の `move`/`result` 行のみ）。
    Jsonl {
        file_idx: usize,
        game_id: u32,
        start_offset: u64,
        end_offset: u64,
    },
    /// CSA ファイル横断インデックスの中での位置。CSA は 1 ファイル = 1 対局なので
    /// `file_idx` が対局を一意に定め、`ordinal` は表示用の通し番号（0-indexed）。
    Csa { file_idx: usize, ordinal: u32 },
}

/// 対局の勝者（手番に依存しない固定 POV で表現する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameOutcomeView {
    Win(Color),
    Draw,
}

/// 対局ファイル単位のメタ情報（JSONL のペアファイル / CSA の単一ファイル）。対局ごと
/// に複製しない（`GameIndexEntry::file_idx` からこちらを引く）。`black_label`/`white_label`
/// は JSONL はエンジンラベル、CSA は `N+`/`N-` のプレイヤー名。
#[derive(Debug, Clone)]
pub struct PairFileMeta {
    pub path: PathBuf,
    pub black_label: String,
    pub white_label: String,
    /// ファイル名から抽出した対局日時キー（`YYYYMMDDHHMMSS` の数値）。日付ソート・
    /// `date:` フィルタ用。ファイル名に日時が無い出典（tournament ペアファイル等)は `None`。
    pub date_key: Option<u64>,
}

/// ファイル名から対局日時キー（`YYYYMMDDHHMMSS`）を抽出する。対応する形式:
/// - csa_client 記録: `20260707_010203_A_vs_B.(csa|jsonl)`（先頭 `YYYYMMDD_HHMMSS`）
/// - wdoor floodgate: `wdoor+floodgate-300-10F+A+B+20260707010203.csa`（末尾 14 桁）
pub fn date_key_from_filename(name: &str) -> Option<u64> {
    let stem = name
        .strip_suffix(".csa")
        .or_else(|| name.strip_suffix(".jsonl"))
        .unwrap_or(name);
    // 先頭 `YYYYMMDD_HHMMSS`
    let head: &str = stem.get(..15).unwrap_or(stem);
    if head.len() == 15
        && head.as_bytes()[8] == b'_'
        && head[..8].bytes().all(|b| b.is_ascii_digit())
        && head[9..].bytes().all(|b| b.is_ascii_digit())
    {
        return format!("{}{}", &head[..8], &head[9..]).parse().ok();
    }
    // 末尾 14 桁（wdoor の `+YYYYMMDDHHMMSS`）
    let tail = stem.rsplit('+').next().unwrap_or("");
    if tail.len() == 14 && tail.bytes().all(|b| b.is_ascii_digit()) {
        return tail.parse().ok();
    }
    None
}

/// live 再読込判定用に、ファイル集合の (パス, サイズ, mtime) をハッシュする。
/// 中身は読まない(stat のみ)。stat に失敗したファイルはパスのみで算入する
/// (書き込み途中の削除等でも fingerprint は変わり、再読込は発火する)。
pub fn fingerprint_paths(paths: &[PathBuf]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for path in paths {
        path.hash(&mut h);
        if let Ok(md) = std::fs::metadata(path) {
            md.len().hash(&mut h);
            if let Ok(mtime) = md.modified()
                && let Ok(d) = mtime.duration_since(std::time::UNIX_EPOCH)
            {
                (d.as_secs(), d.subsec_nanos()).hash(&mut h);
            }
        }
    }
    h.finish()
}

/// 索引全体。`entries` は出典を問わず1つの横断リストとしてフラット化されている。
#[derive(Debug, Clone, Default)]
pub struct GameIndex {
    pub entries: Vec<GameIndexEntry>,
    /// JSONL のみ使用。PSV ソースの場合は空。
    pub pair_files: Vec<PairFileMeta>,
    /// 致命的ではないが利用者に伝えるべき事項（シャッフル済み PSV の疑い、
    /// 対局データを含まないため読み飛ばした JSONL ファイル等）。
    pub warnings: Vec<String>,
}

impl GameIndex {
    pub fn pair_file(&self, file_idx: usize) -> Option<&PairFileMeta> {
        self.pair_files.get(file_idx)
    }
}

/// 1局を開いたときに遅延構築する、再生用の完全な手順。
#[derive(Debug, Clone)]
pub struct GameRecord {
    pub moves: Vec<MoveView>,
    /// 先頭の手数が 1 より大きいとき、それを「記録の欠落」(⋯N手欠落⋯) として
    /// 表示するか。PSV は `skip_initial_ply` で先頭手が落ちうるので true。JSONL の
    /// 定跡開始局面（例: 24手目から）は正当な開始で欠落ではないため false。
    pub leading_gap_is_drop: bool,
}

#[derive(Debug, Clone)]
pub struct MoveView {
    /// 局面の絶対手数。PSV は `game_ply`、JSONL は `sfen_before` の SFEN 手数カウンタから
    /// 採る（JSONL の対局内 1 始まり `ply` は使わない。定跡途中開始でも正しい手数にするため）。
    /// PSV は `skip_initial_ply`/`skip_in_check` により欠番がありうるので連番の保証はない
    /// （欠番はそのまま表示する）。
    pub ply: u32,
    pub side: Color,
    pub sfen_before: String,
    pub mv: Move,
    /// `kif.rs::format_move_label` を再利用した、棋譜風の人間可読ラベル。
    pub kif_label: String,
    pub annotation: MoveAnnotation,
}

/// 手への注釈。全フィールド `Option`。PSV は `score_cp` のみ埋まり、
/// JSONL（tournament 出力）は埋まる分だけ多くなる。
#[derive(Debug, Clone, Default)]
pub struct MoveAnnotation {
    pub score_cp: Option<i32>,
    pub score_mate: Option<i32>,
    pub depth: Option<u32>,
    pub seldepth: Option<u32>,
    pub nodes: Option<u64>,
    pub nps: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub timed_out: Option<bool>,
}

/// PSV / JSONL の違いを吸収する共通インターフェース。
pub trait GameSource {
    /// 全件を1パスでストリーミングし、対局単位のメタ情報だけを集めた索引を返す。
    fn build_index(&self) -> Result<GameIndex>;

    /// 索引のオフセットへ seek し、その対局の範囲だけを読んで再生用の手順を返す。
    /// `index` は `file_idx` から `PairFileMeta`（出典パス）を引くために使う
    /// （`GameIndexEntry` 自体にはパスを複製しない）。PSV ソースでは未使用。
    fn load_game(&self, index: &GameIndex, entry: &GameIndexEntry) -> Result<GameRecord>;

    /// live 再読込用の軽量フィンガープリント。対象ファイル集合の (パス, サイズ, mtime)
    /// のみから計算し、中身は読まない(数百ファイルでも stat だけで済む)。値が前回と
    /// 変わったときだけ `build_index` を取り直す、が想定用途。`None` は live 非対応
    /// (PSV 等)。既定実装は非対応。
    fn live_fingerprint(&self) -> Result<Option<u64>> {
        Ok(None)
    }
}

/// 対局一覧に出す表示ラベルを、保持済みの文字列ではなくその場で組み立てる
/// （`display_label: String` を `GameIndexEntry` ごとに複製しない設計上の選択）。
pub fn display_label(index: &GameIndex, entry: &GameIndexEntry) -> String {
    match entry.source {
        GameSourceRef::Psv { ordinal, .. } => format!("psv #{:03}", ordinal + 1),
        GameSourceRef::Jsonl {
            file_idx, game_id, ..
        } => match index.pair_file(file_idx) {
            Some(meta) => format!("{}-vs-{} #{:03}", meta.black_label, meta.white_label, game_id),
            None => format!("?-vs-? #{:03}", game_id),
        },
        GameSourceRef::Csa { file_idx, ordinal } => match index.pair_file(file_idx) {
            Some(meta) => {
                format!("{}-vs-{} #{:03}", meta.black_label, meta.white_label, ordinal + 1)
            }
            None => format!("?-vs-? #{:03}", ordinal + 1),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_key_from_filename_supported_formats() {
        // csa_client 記録形式(先頭 YYYYMMDD_HHMMSS)
        assert_eq!(
            date_key_from_filename("20260707_010203_RAMU_TF_vs_Suisho.csa"),
            Some(20260707010203)
        );
        assert_eq!(date_key_from_filename("20260707_010203_A_vs_B.jsonl"), Some(20260707010203));
        // wdoor floodgate 形式(末尾 14 桁)
        assert_eq!(
            date_key_from_filename("wdoor+floodgate-300-10F+A+B+20260707010203.csa"),
            Some(20260707010203)
        );
        // 日時を持たない出典
        assert_eq!(date_key_from_filename("rsA-vs-rsB.jsonl"), None);
        assert_eq!(date_key_from_filename("2026_bad.csa"), None);
    }
}
