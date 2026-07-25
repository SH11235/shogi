//! 宣言ルール距離ペア順序一致指標の構築・採点ツール。
//!
//! `%KACHI`（入玉宣言勝ち）で終局した対局の勝者手番局面から、「宣言成立へのルール距離が
//! 確定的に縮んだ」隣接局面 pair を抽出し（`build-pairs`）、NNUE 静的評価が後局面を
//! 前局面より高く評価するか（順序一致率）を条件別に測る（`eval-pairs`）。
//!
//! CSA replay と終局特殊手の取得は `replay::csa_source::CsaSource` に委譲する。
//! ルール特徴は `Position::entering_king_point_info` / `Position::in_check` のみを使い、
//! 評価値を母集団選別に使わない（循環の回避）。

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rshogi_core::nnue::{
    AccumulatorStackVariant, LayerStackBucketMode, LayerStacksAccCache, evaluate_dispatch,
    get_network, init_nnue, load_progress_coeff_kpabs, set_layer_stack_bucket_mode,
    set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, EnteringKingRule, Move};
use rshogi_csa::SpecialMove;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::replay::csa_source::CsaSource;
use crate::replay::model::{
    GameIndex, GameIndexEntry, GameOutcomeView, GameSource, GameSourceRef, MoveView,
};

const DEFAULT_BOOTSTRAP: u32 = 10_000;
const DEFAULT_SEED: u64 = 20_260_726;

#[derive(Parser, Debug)]
#[command(
    name = "nyugyoku_metrics",
    version,
    about = "宣言ルール距離ペアを CSA から抽出し、NNUE 静的評価の順序一致率を採点する"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// %KACHI 終局対局の勝者手番局面から宣言ルール距離ペアを抽出する。
    BuildPairs(BuildArgs),
    /// pairs.jsonl を native NNUE 静的評価で採点し、順序一致率を条件別に集計する。
    EvalPairs(EvalArgs),
}

#[derive(Parser, Debug)]
struct BuildArgs {
    /// 入力 CSA ファイルまたは CSA ディレクトリ。
    #[arg(long)]
    input: PathBuf,
    /// 出力ディレクトリ（pairs.jsonl / meta.json を書く）。
    #[arg(long)]
    out_dir: PathBuf,
}

#[derive(Parser, Debug)]
struct EvalArgs {
    /// `nyugyoku_metrics build-pairs` が出した pairs.jsonl。
    #[arg(long)]
    pairs: PathBuf,
    /// NNUE ファイル。
    #[arg(long)]
    eval_file: PathBuf,
    /// LayerStacks progress8kpabs 用 progress.bin（--bucket-mode progress8kpabs で必須）。
    #[arg(long)]
    progress_file: Option<PathBuf>,
    /// LayerStacks の bucket 選択モード。
    #[arg(long, value_enum, default_value_t = BucketModeArg::Progress8kpabs)]
    bucket_mode: BucketModeArg,
    /// metrics.json の出力先。
    #[arg(long)]
    out: PathBuf,
    /// 対局クラスタ bootstrap の replicate 数（0 で CI を出さない）。
    #[arg(long, default_value_t = DEFAULT_BOOTSTRAP)]
    bootstrap: u32,
    /// bootstrap 乱数 seed（同 seed・同入力で CI は bit 一致する）。
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// pair ごとの (eval_before, eval_after, agreement) を jsonl で出力する先。
    #[arg(long)]
    dump_pairs: Option<PathBuf>,
}

/// LayerStacks bucket mode の CLI 表現。
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum BucketModeArg {
    /// 進行度方式（progress.bin 必須）。ek_testset eval と同じ既定。
    Progress8kpabs,
    /// 両玉相対段方式（progress.bin 不要）。
    Kingrank9,
}

/// pair の遷移条件（本指標の 4 条件、固定順）。
///
/// いずれも「宣言成立へのルール距離が確定的に縮んだ」ことのルールベース判定で、
/// 評価値には依存しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Condition {
    /// 宣言点（27点法の駒点）が増えた。
    PointGain,
    /// 敵陣三段内の駒数（玉を除く）が増えた。
    ZonePieceGain,
    /// 勝者玉が敵陣三段内へ入った（false→true）。
    KingEntry,
    /// 勝者玉への王手が解除された（true→false）。
    CheckResolved,
}

impl Condition {
    const ALL: [Condition; 4] = [
        Condition::PointGain,
        Condition::ZonePieceGain,
        Condition::KingEntry,
        Condition::CheckResolved,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Condition::PointGain => "point_gain",
            Condition::ZonePieceGain => "zone_piece_gain",
            Condition::KingEntry => "king_entry",
            Condition::CheckResolved => "check_resolved",
        }
    }

    fn index(self) -> usize {
        match self {
            Condition::PointGain => 0,
            Condition::ZonePieceGain => 1,
            Condition::KingEntry => 2,
            Condition::CheckResolved => 3,
        }
    }
}

/// pairs.jsonl の 1 行（1 pair）。両局面とも勝者手番（同一 POV）。
#[derive(Debug, Serialize, Deserialize, Clone)]
struct PairRecord {
    source_csa: String,
    /// 対局日時キー（ファイル名由来の `YYYYMMDDHHMMSS`）。将来の時期別 group split 用。
    /// ファイル名に日時を持たない出典は `None`。
    date_key: Option<u64>,
    /// 先手プレイヤー名（CSA `N+` の生値）。将来のエンジン別 group split 用。
    black_engine: String,
    /// 後手プレイヤー名（CSA `N-` の生値）。
    white_engine: String,
    /// 勝者（宣言側）。'b' / 'w'。
    winner: char,
    ply_before: u32,
    ply_after: u32,
    sfen_before: String,
    sfen_after: String,
    /// 成立した遷移条件（1 つ以上）。
    conditions: Vec<Condition>,
    points_before: u32,
    points_after: u32,
    zone_before: u32,
    zone_after: u32,
    king_in_before: bool,
    king_in_after: bool,
    check_before: bool,
    check_after: bool,
}

#[derive(Debug, Serialize)]
struct BuildMeta {
    input: String,
    /// 走査した CSA ファイル数（1 ファイル = 1 対局）。
    games_scanned: usize,
    /// `%KACHI` 終局と確認できた対局数。
    kachi_games: usize,
    /// 勝者の突き合わせ（parse 由来の勝者 vs replay 終端手番）に失敗して除外した対局数。
    games_skipped_winner_mismatch: usize,
    /// replay 未完走・SFEN 復元不能などで終端局面を復元できず除外した対局数。
    games_skipped_broken: usize,
    /// `%KACHI` 記録だが終端局面で Point27 宣言が成立せず除外した対局数
    /// （宣言失敗 = illegal kachi、または別ルール運用の対局）。
    games_skipped_point27_mismatch: usize,
    /// 1 pair 以上を出力した対局数。
    games_with_pairs: usize,
    pairs_total: usize,
    /// 条件別の pair 数と対局数（クラスタ数）。
    conditions: BTreeMap<String, ConditionBuildCount>,
}

#[derive(Debug, Serialize)]
struct ConditionBuildCount {
    pairs: usize,
    games: usize,
}

/// eval-pairs の集計出力（全体 + 条件別）。
#[derive(Debug, Serialize)]
struct EvalPairsMetrics {
    pairs: String,
    eval_file: String,
    progress_file: Option<String>,
    bucket_mode: String,
    bootstrap: u32,
    seed: u64,
    overall: SliceMetrics,
    conditions: BTreeMap<String, SliceMetrics>,
}

/// 1 スライス（全体または 1 条件）の順序一致率と対局クラスタ bootstrap の 95% CI。
#[derive(Debug, Serialize, Clone, PartialEq)]
struct SliceMetrics {
    /// 順序一致率（tie は 0.5）。pair が 1 件も無いスライスは `None`。
    agreement: Option<f64>,
    n_pairs: u64,
    n_games: usize,
    ci95_lo: Option<f64>,
    ci95_hi: Option<f64>,
}

/// dump-pairs の 1 行。
#[derive(Debug, Serialize)]
struct DumpRecord<'a> {
    source_csa: &'a str,
    ply_before: u32,
    ply_after: u32,
    conditions: &'a [Condition],
    eval_before: i32,
    eval_after: i32,
    agreement: f64,
}

/// CLI entrypoint。
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::BuildPairs(args) => run_build(&args),
        Command::EvalPairs(args) => run_eval(&args),
    }
}

// ---------------------------------------------------------------------------
// build-pairs
// ---------------------------------------------------------------------------

fn run_build(args: &BuildArgs) -> Result<()> {
    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("出力ディレクトリを作成できません: {}", args.out_dir.display()))?;
    let pairs_path = args.out_dir.join("pairs.jsonl");
    let meta_path = args.out_dir.join("meta.json");
    let mut pairs_out = BufWriter::new(File::create(&pairs_path)?);

    let mut games_scanned = 0usize;
    let mut kachi_games = 0usize;
    let mut games_skipped_winner_mismatch = 0usize;
    let mut games_skipped_broken = 0usize;
    let mut games_skipped_point27_mismatch = 0usize;
    let mut games_with_pairs = 0usize;
    let mut pairs_total = 0usize;
    let mut cond_pairs = [0usize; Condition::ALL.len()];
    let mut cond_games = [0usize; Condition::ALL.len()];

    // 対局ごとの index やパス一覧をコーパス全体分保持しないよう、CSA を決定的な順序の
    // 遅延走査で 1 ファイル = 1 対局ずつ処理する（ピークメモリは対局数に非依存）。
    for path in csa_paths(&args.input)? {
        let path = path?;
        games_scanned += 1;

        // 高速化: `%KACHI` を含まないファイルは宣言終局ではあり得ないので parse せず skip
        // する。実測 (floodgate 混合 9,952 局、kachi 率 9.1%、Windows NVMe): プレフィルタ
        // あり 1.58 秒 / なし 8.0 秒で約 5 倍差 (非宣言局の parse 回避が、宣言局の
        // 二重読みのコストを大きく上回る)。
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "warning: {}: 読み込みに失敗したため読み飛ばしました（{e}）",
                    path.display()
                );
                continue;
            }
        };
        // parser は手番付き `%+KACHI` / `%-KACHI` も `SpecialMove::Win` に受理するため、
        // プレフィルタは接頭辞なしの部分文字列で判定する（偽陽性は後段の parse が落とす）。
        if !text.contains("KACHI") {
            continue;
        }

        let source = CsaSource::new(&path);
        let index = source.build_index()?;
        for warning in &index.warnings {
            eprintln!("warning: {warning}");
        }
        for entry in &index.entries {
            let game = source.load_game(&index, entry)?;
            // 宣言勝ち終局のみ対象（`%KACHI` がコメント等に現れただけのファイルはここで落ちる）。
            if game.termination != Some(SpecialMove::Win) {
                continue;
            }
            kachi_games += 1;
            let prov = pair_provenance(&index, entry)?;
            let source_csa = prov.source_csa.as_str();

            // 勝者 = 宣言側 = 終端で手番だった側（derive_outcome の Win 側と一致するはず）。
            let Some(GameOutcomeView::Win(winner)) = entry.outcome else {
                games_skipped_winner_mismatch += 1;
                eprintln!(
                    "warning: {source_csa}: %KACHI 終局なのに勝者を導出できないため除外します"
                );
                continue;
            };
            // 終端局面の突き合わせには replay の完走（全手が通常手として盤面追跡できたこと）
            // が必要。完走しなかった対局は終端局面を復元できないため broken として除外する。
            let replay_complete = game.moves.len() as u64 == u64::from(entry.ply_count)
                && game.moves.iter().all(|m| m.mv.is_normal());
            if !replay_complete {
                games_skipped_broken += 1;
                eprintln!(
                    "warning: {source_csa}: replay が完走せず終端局面を復元できないため除外します"
                );
                continue;
            }
            // 最終手（敗者の手のはず）と宣言側の突き合わせ。
            if let Some(last) = game.moves.last()
                && last.side == winner
            {
                games_skipped_winner_mismatch += 1;
                eprintln!("warning: {source_csa}: 宣言側と終端手番が一致しないため除外します");
                continue;
            }
            // wdoor shogi-server は宣言失敗（illegal kachi = 宣言側の負け）でも棋譜へ
            // `%KACHI` を書くため、終端局面（宣言側手番）で Point27 宣言が実際に成立するか
            // を評価し、成立しない対局は pair 化から除外する。
            let terminal = match terminal_position(&game.moves) {
                Ok(pos) => pos,
                Err(e) => {
                    games_skipped_broken += 1;
                    eprintln!(
                        "warning: {source_csa}: 終端局面を復元できないため除外します（{e:#}）"
                    );
                    continue;
                }
            };
            if terminal.declaration_win(EnteringKingRule::Point27) == Move::NONE {
                games_skipped_point27_mismatch += 1;
                eprintln!(
                    "warning: {source_csa}: %KACHI 記録だが終端局面で Point27 宣言が成立しないため除外します（宣言失敗または別ルールの対局）"
                );
                continue;
            }

            let pairs = match build_pairs_for_game(&game.moves, winner, &prov) {
                Ok(pairs) => pairs,
                Err(e) => {
                    games_skipped_broken += 1;
                    eprintln!("warning: {source_csa}: pair を構築できないため除外します（{e:#}）");
                    continue;
                }
            };
            if pairs.is_empty() {
                continue;
            }
            games_with_pairs += 1;
            let mut game_has_cond = [false; Condition::ALL.len()];
            for pair in &pairs {
                for cond in &pair.conditions {
                    cond_pairs[cond.index()] += 1;
                    game_has_cond[cond.index()] = true;
                }
                serde_json::to_writer(&mut pairs_out, pair)?;
                writeln!(pairs_out)?;
                pairs_total += 1;
            }
            for cond in Condition::ALL {
                if game_has_cond[cond.index()] {
                    cond_games[cond.index()] += 1;
                }
            }
        }
    }
    pairs_out.flush()?;

    let conditions = Condition::ALL
        .into_iter()
        .map(|cond| {
            (
                cond.as_str().to_string(),
                ConditionBuildCount {
                    pairs: cond_pairs[cond.index()],
                    games: cond_games[cond.index()],
                },
            )
        })
        .collect();
    let meta = BuildMeta {
        input: args.input.display().to_string(),
        games_scanned,
        kachi_games,
        games_skipped_winner_mismatch,
        games_skipped_broken,
        games_skipped_point27_mismatch,
        games_with_pairs,
        pairs_total,
        conditions,
    };
    write_json_pretty(&meta_path, &meta)?;

    eprintln!(
        "wrote {pairs_total} pairs from {games_with_pairs} games (kachi={kachi_games}, scanned={games_scanned}) to {}",
        pairs_path.display()
    );
    Ok(())
}

/// 入力がディレクトリなら配下の `*.csa` を、単一ファイルならそれ 1 つを列挙する。
///
/// ek_testset の同名ヘルパと同じ設計（ek_testset 側は変更しない方針のため複製）:
/// 全パスを収集・保持せず、ディレクトリごとにファイル名ソートした DFS で遅延走査する
/// （走査順は決定的、ピークメモリは総ファイル数に非依存）。`follow_links(false)` で
/// symlink は辿らない。走査エラーは握りつぶさず `Err` として返す。
fn csa_paths(input: &Path) -> Result<Box<dyn Iterator<Item = Result<PathBuf>>>> {
    let md = fs::metadata(input)
        .with_context(|| format!("入力を確認できません: {}", input.display()))?;
    if md.is_dir() {
        Ok(Box::new(
            WalkDir::new(input)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_map(|entry| match entry {
                    Ok(e) => (e.file_type().is_file()
                        && e.path().extension().and_then(|x| x.to_str()) == Some("csa"))
                    .then(|| Ok(e.into_path())),
                    Err(e) => Some(Err(anyhow!(e).context("入力ディレクトリの走査に失敗しました"))),
                }),
        ))
    } else {
        Ok(Box::new(std::iter::once(Ok(input.to_path_buf()))))
    }
}

/// `%KACHI` 対局の終端局面（宣言側手番）を、最終 normal 手を `sfen_before` へ適用して
/// 復元する。replay が完走していない（最終手が `Move::NONE` 等の）対局には使えない。
fn terminal_position(moves: &[MoveView]) -> Result<Position> {
    let last = moves.last().ok_or_else(|| anyhow!("指し手がありません"))?;
    if !last.mv.is_normal() {
        bail!("最終手が通常手ではありません");
    }
    let mut pos = Position::new();
    pos.set_sfen(&last.sfen_before)
        .with_context(|| format!("SFEN を復元できません: {}", last.sfen_before))?;
    let gives_check = pos.gives_check(last.mv);
    pos.do_move(last.mv, gives_check);
    Ok(pos)
}

/// 局面のルール特徴（すべて勝者視点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositionFeatures {
    points: u32,
    zone_pieces: u32,
    king_in_enemy: bool,
    in_check: bool,
}

fn position_features(sfen: &str, winner: Color) -> Result<PositionFeatures> {
    let mut pos = Position::new();
    pos.set_sfen(sfen).with_context(|| format!("SFEN を復元できません: {sfen}"))?;
    if pos.side_to_move() != winner {
        bail!("勝者手番のはずの局面で手番が一致しません: {sfen}");
    }
    let info = pos.entering_king_point_info(winner);
    Ok(PositionFeatures {
        points: info.points,
        zone_pieces: info.enemy_zone_pieces,
        king_in_enemy: info.king_in_enemy,
        // 勝者手番なので in_check() は勝者玉への王手状態。
        in_check: pos.in_check(),
    })
}

/// 成立した遷移条件を固定順で返す（1 つも無ければ空）。
fn transition_conditions(before: &PositionFeatures, after: &PositionFeatures) -> Vec<Condition> {
    let mut out = Vec::new();
    if after.points > before.points {
        out.push(Condition::PointGain);
    }
    if after.zone_pieces > before.zone_pieces {
        out.push(Condition::ZonePieceGain);
    }
    if !before.king_in_enemy && after.king_in_enemy {
        out.push(Condition::KingEntry);
    }
    if before.in_check && !after.in_check {
        out.push(Condition::CheckResolved);
    }
    out
}

/// 隣接する勝者手番局面 pair の moves index (i, i+2) を列挙する。
///
/// 採用条件: 両端が勝者手番・ply 差がちょうど 2・間に相手の通常手が 1 手だけ挟まる。
/// 前局面の手または間の相手手が `Move::NONE`（parse fallback）の場合は盤面遷移を
/// 信頼できないので skip する（後局面は `sfen_before` しか使わないため手自体は不問）。
fn adjacent_winner_pair_indices(moves: &[MoveView], winner: Color) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (i, w) in moves.windows(3).enumerate() {
        let [a, mid, b] = w else {
            continue;
        };
        if a.side != winner || b.side != winner || mid.side == winner {
            continue;
        }
        if !a.mv.is_normal() || !mid.mv.is_normal() {
            continue;
        }
        // ply 欠番（PSV 由来等）は隣接とみなさない。
        if mid.ply.checked_sub(a.ply) != Some(1) || b.ply.checked_sub(a.ply) != Some(2) {
            continue;
        }
        out.push((i, i + 2));
    }
    out
}

/// 1 対局から条件付き pair を構築する（条件が 1 つも成立しない pair は出力しない）。
fn build_pairs_for_game(
    moves: &[MoveView],
    winner: Color,
    prov: &PairProvenance,
) -> Result<Vec<PairRecord>> {
    let mut out = Vec::new();
    for (i, j) in adjacent_winner_pair_indices(moves, winner) {
        let before = position_features(&moves[i].sfen_before, winner)?;
        let after = position_features(&moves[j].sfen_before, winner)?;
        let conditions = transition_conditions(&before, &after);
        if conditions.is_empty() {
            continue;
        }
        out.push(PairRecord {
            source_csa: prov.source_csa.clone(),
            date_key: prov.date_key,
            black_engine: prov.black_engine.clone(),
            white_engine: prov.white_engine.clone(),
            winner: color_label(winner),
            ply_before: moves[i].ply,
            ply_after: moves[j].ply,
            sfen_before: moves[i].sfen_before.clone(),
            sfen_after: moves[j].sfen_before.clone(),
            conditions,
            points_before: before.points,
            points_after: after.points,
            zone_before: before.zone_pieces,
            zone_after: after.zone_pieces,
            king_in_before: before.king_in_enemy,
            king_in_after: after.king_in_enemy,
            check_before: before.in_check,
            check_after: after.in_check,
        });
    }
    Ok(out)
}

/// pair 出力へ複製する対局単位の出典情報（将来の時期×エンジン group split 用の生値）。
#[derive(Debug, Clone)]
struct PairProvenance {
    source_csa: String,
    /// ファイル名から抽出した対局日時キー（`YYYYMMDDHHMMSS`）。無い出典は `None`。
    date_key: Option<u64>,
    /// CSA `N+` のプレイヤー名。
    black_engine: String,
    /// CSA `N-` のプレイヤー名。
    white_engine: String,
}

fn pair_provenance(index: &GameIndex, entry: &GameIndexEntry) -> Result<PairProvenance> {
    let GameSourceRef::Csa { file_idx, .. } = entry.source else {
        bail!("CSA 以外の GameIndexEntry が渡されました");
    };
    let meta = index
        .pair_file(file_idx)
        .ok_or_else(|| anyhow!("file_idx {file_idx} が index にありません"))?;
    Ok(PairProvenance {
        source_csa: meta.path.display().to_string(),
        date_key: meta.date_key,
        black_engine: meta.black_label.clone(),
        white_engine: meta.white_label.clone(),
    })
}

fn color_label(c: Color) -> char {
    match c {
        Color::Black => 'b',
        Color::White => 'w',
    }
}

fn color_from_label(c: char) -> Result<Color> {
    match c {
        'b' => Ok(Color::Black),
        'w' => Ok(Color::White),
        _ => bail!("winner は 'b' か 'w' を指定してください: {c:?}"),
    }
}

// ---------------------------------------------------------------------------
// eval-pairs
// ---------------------------------------------------------------------------

/// 集計スロット数（0 = 全体、1.. = `Condition::ALL` の各条件）。
const SLOTS: usize = Condition::ALL.len() + 1;

/// 対局 1 件の (agreement 和, pair 数)。対局クラスタ bootstrap の resample 単位。
#[derive(Debug, Clone, Copy, Default)]
struct GameAgg {
    sum: f64,
    count: u64,
}

/// pair を対局クラスタ単位で集計する。
///
/// per-game 集計（`HashMap` + `Vec`）を常駐させるのは意図的な設計判断:
/// 対局クラスタ bootstrap には per-cluster（= 対局単位）の集計保持が本質的に必要で、
/// ピークメモリは pair 数ではなくクラスタ数 = 対局数に比例する。全 floodgate 規模
/// （約 7 万局）でも数 MB に収まり、streaming 規約が禁じる「入力件数線形の load-all」
/// には当たらない（pair 自体はストリーミングで読み捨てる）。
#[derive(Default)]
struct PairAggregator {
    /// source_csa → 対局 index（初出順で採番。入力順が固定なら決定的）。
    game_index: HashMap<String, usize>,
    /// 対局ごとの [全体, 条件別...] 集計。
    games: Vec<[GameAgg; SLOTS]>,
}

impl PairAggregator {
    fn push(&mut self, source_csa: &str, conditions: &[Condition], agreement: f64) {
        let idx = match self.game_index.get(source_csa) {
            Some(&idx) => idx,
            None => {
                let idx = self.games.len();
                self.game_index.insert(source_csa.to_string(), idx);
                self.games.push([GameAgg::default(); SLOTS]);
                idx
            }
        };
        let slots = &mut self.games[idx];
        slots[0].sum += agreement;
        slots[0].count += 1;
        for cond in conditions {
            let slot = &mut slots[cond.index() + 1];
            slot.sum += agreement;
            slot.count += 1;
        }
    }

    /// スロットごとに agreement 率と対局クラスタ bootstrap の 95% CI を確定する。
    ///
    /// bootstrap の seed はスロットごとに `slot_seed` で導出する（スロット間で
    /// 独立な決定的ストリーム）。
    fn finish(&self, bootstrap: u32, seed: u64) -> (SliceMetrics, BTreeMap<String, SliceMetrics>) {
        let overall = self.slice_metrics(0, bootstrap, slot_seed(seed, 0));
        let conditions = Condition::ALL
            .into_iter()
            .map(|cond| {
                let slot = cond.index() + 1;
                (
                    cond.as_str().to_string(),
                    self.slice_metrics(slot, bootstrap, slot_seed(seed, slot)),
                )
            })
            .collect();
        (overall, conditions)
    }

    fn slice_metrics(&self, slot: usize, bootstrap: u32, seed: u64) -> SliceMetrics {
        // pair を 1 件以上持つ対局だけをこのスライスのクラスタ集合とする（対局 index 順）。
        let per_game: Vec<GameAgg> =
            self.games.iter().map(|g| g[slot]).filter(|g| g.count > 0).collect();
        let n_pairs: u64 = per_game.iter().map(|g| g.count).sum();
        let sum: f64 = per_game.iter().map(|g| g.sum).sum();
        let ci = bootstrap_ci95(&per_game, bootstrap, seed);
        SliceMetrics {
            agreement: (n_pairs > 0).then(|| sum / n_pairs as f64),
            n_pairs,
            n_games: per_game.len(),
            ci95_lo: ci.map(|(lo, _)| lo),
            ci95_hi: ci.map(|(_, hi)| hi),
        }
    }
}

/// スロット別 bootstrap seed の派生。
///
/// `seed + slot` のような加算派生は、近い seed 同士（例: `seed=42, slot=1` と
/// `seed=43, slot=0`）でストリームが衝突しうるため、黄金比由来の大定数
/// （splitmix64 の増分）を乗じてビット全域へ拡散させる。slot 0（全体）は `seed` のまま。
fn slot_seed(seed: u64, slot: usize) -> u64 {
    seed ^ (slot as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// 昇順ソート済み標本の分位点。0 始まりの index `round((n - 1) * q)` の要素を返す
/// （round は四捨五入 = 半端は 0 から遠い側へ。nearest-rank 法 `ceil(n * q) - 1` とは
/// 異なり、線形補間もしない）。
fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// 対局クラスタ bootstrap による agreement 率の 95% CI
/// （replicate 統計量の `percentile_sorted` による q=0.025 / 0.975 分位点）。
///
/// 各 replicate で `per_game` から同数の対局を復元抽出し、pair 重み付きの agreement 率を
/// 計算する。`per_game` の各対局は pair を 1 件以上持つ前提（分母 0 は起きない）。
/// seed 固定 + 入力順固定で結果は bit 一致する。対局が 0 件、または `replicates == 0`
/// のときは `None`。
fn bootstrap_ci95(per_game: &[GameAgg], replicates: u32, seed: u64) -> Option<(f64, f64)> {
    if per_game.is_empty() || replicates == 0 {
        return None;
    }
    let n = per_game.len();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut stats = Vec::with_capacity(replicates as usize);
    for _ in 0..replicates {
        let mut sum = 0.0f64;
        let mut count = 0u64;
        for _ in 0..n {
            let g = &per_game[rng.random_range(0..n)];
            sum += g.sum;
            count += g.count;
        }
        stats.push(sum / count as f64);
    }
    stats.sort_by(f64::total_cmp);
    Some((percentile_sorted(&stats, 0.025), percentile_sorted(&stats, 0.975)))
}

/// 順序一致スコア。後局面（距離小）を前局面より高く評価すれば 1、tie は 0.5。
fn agreement_score(eval_before: i32, eval_after: i32) -> f64 {
    match eval_after.cmp(&eval_before) {
        std::cmp::Ordering::Greater => 1.0,
        std::cmp::Ordering::Equal => 0.5,
        std::cmp::Ordering::Less => 0.0,
    }
}

fn run_eval(args: &EvalArgs) -> Result<()> {
    match args.bucket_mode {
        BucketModeArg::Progress8kpabs => {
            let progress_file = args.progress_file.as_ref().ok_or_else(|| {
                anyhow!("--bucket-mode progress8kpabs では --progress-file が必須です")
            })?;
            let weights = load_progress_coeff_kpabs(progress_file)
                .map_err(|e| anyhow!("progress 読み込みに失敗しました: {e}"))?;
            set_layer_stack_progress_kpabs_weights(weights)
                .map_err(|e| anyhow!("progress 設定に失敗しました: {e}"))?;
            set_layer_stack_bucket_mode(LayerStackBucketMode::Progress8KPAbs);
        }
        BucketModeArg::Kingrank9 => {
            if args.progress_file.is_some() {
                bail!("--bucket-mode kingrank9 では --progress-file は使いません");
            }
            set_layer_stack_bucket_mode(LayerStackBucketMode::KingRank9);
        }
    }
    init_nnue(&args.eval_file)
        .with_context(|| format!("NNUE を読み込めません: {}", args.eval_file.display()))?;
    let network = get_network().ok_or_else(|| anyhow!("NNUE が初期化されていません"))?;
    if !network.is_layer_stacks() {
        bail!(
            "nyugyoku_metrics eval-pairs は LayerStacks NNUE のみ対応しています: {}",
            network.architecture_name()
        );
    }
    let mut stack = AccumulatorStackVariant::from_network(&network);
    // acc_cache (Finny Tables) は静的 LayerStacks variant 専用の API で、
    // runtime-dimensions ビルドでは同じ net でも DynamicLayerStacks として load され
    // `as_layer_stacks()` が panic する (`is_layer_stacks()` は Dynamic でも true)。
    // `evaluate_dispatch` は None でも全 variant を正しく評価するため cache は使わない。
    let mut acc_cache: Option<LayerStacksAccCache> = None;

    let mut dump = args
        .dump_pairs
        .as_ref()
        .map(|path| {
            File::create(path)
                .map(BufWriter::new)
                .with_context(|| format!("出力できません: {}", path.display()))
        })
        .transpose()?;

    let mut agg = PairAggregator::default();
    let file = File::open(&args.pairs)
        .with_context(|| format!("pairs を開けません: {}", args.pairs.display()))?;
    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let at = || format!("{}:{}", args.pairs.display(), line_no + 1);
        let pair: PairRecord =
            serde_json::from_str(&line).with_context(|| format!("{}: JSON を読めません", at()))?;
        let winner = color_from_label(pair.winner).with_context(at)?;

        let mut eval_at = |sfen: &str| -> Result<i32> {
            let mut pos = Position::new();
            pos.set_sfen(sfen).with_context(|| format!("{}: SFEN を読めません", at()))?;
            if pos.side_to_move() != winner {
                bail!("{}: 勝者手番でない局面が pair に含まれています", at());
            }
            stack.reset();
            // ek_testset と同じく cp（歩=100）で扱う。順序一致は狭義単調変換に不変なので
            // fv_scale（定数倍）には依存しない。
            Ok(evaluate_dispatch(&pos, &mut stack, &mut acc_cache).to_cp())
        };
        let eval_before = eval_at(&pair.sfen_before)?;
        let eval_after = eval_at(&pair.sfen_after)?;
        let agreement = agreement_score(eval_before, eval_after);
        agg.push(&pair.source_csa, &pair.conditions, agreement);

        if let Some(dump) = &mut dump {
            let record = DumpRecord {
                source_csa: &pair.source_csa,
                ply_before: pair.ply_before,
                ply_after: pair.ply_after,
                conditions: &pair.conditions,
                eval_before,
                eval_after,
                agreement,
            };
            serde_json::to_writer(&mut *dump, &record)?;
            writeln!(dump)?;
        }
    }
    if let Some(mut dump) = dump {
        dump.flush()?;
    }

    let (overall, conditions) = agg.finish(args.bootstrap, args.seed);
    let out = EvalPairsMetrics {
        pairs: args.pairs.display().to_string(),
        eval_file: args.eval_file.display().to_string(),
        progress_file: args.progress_file.as_ref().map(|p| p.display().to_string()),
        bucket_mode: match args.bucket_mode {
            BucketModeArg::Progress8kpabs => LayerStackBucketMode::Progress8KPAbs.as_str(),
            BucketModeArg::Kingrank9 => LayerStackBucketMode::KingRank9.as_str(),
        }
        .to_string(),
        bootstrap: args.bootstrap,
        seed: args.seed,
        overall,
        conditions,
    };

    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    serde_json::to_writer_pretty(&mut locked, &out)?;
    writeln!(locked)?;
    write_json_pretty(&args.out, &out)?;
    Ok(())
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut writer = BufWriter::new(
        File::create(path).with_context(|| format!("出力できません: {}", path.display()))?,
    );
    serde_json::to_writer_pretty(&mut writer, value)?;
    writeln!(writer)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::model::MoveAnnotation;

    fn write_csa(dir: &Path, name: &str, text: &str) {
        let mut f = File::create(dir.join(name)).expect("create");
        f.write_all(text.as_bytes()).expect("write");
    }

    /// テスト用の出典情報（build_pairs_for_game 直呼び用）。
    fn test_prov(name: &str) -> PairProvenance {
        PairProvenance {
            source_csa: name.to_string(),
            date_key: None,
            black_engine: "B".to_string(),
            white_engine: "W".to_string(),
        }
    }

    /// 先手が王手解除 → 点数/枚数増 → %KACHI で勝つ合成対局。
    /// pair (1,3) = check_resolved、pair (3,5) = point_gain + zone_piece_gain。
    /// 終端（6 手目適用後、先手手番）は Point27 宣言成立形:
    /// 玉 42（敵陣内）・敵陣内 10 枚（飛 2 + 角 2 + 金 4 + 銀 1 + 歩 1）・
    /// 28 点（10 + 大駒加点 4x4 + 持ち歩 2）・王手なし。
    const KACHI_GAINS_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+51OU11HI21HI13KA23KA61KI71KI81KI91KI63GI34FU00FU00FU\n",
        "P-59OU42GI\n",
        "+\n",
        "+5141OU\nT1\n",
        "-4243GI\nT1\n",
        "+3433FU\nT1\n",
        "-4344GI\nT1\n",
        "+4142OU\nT1\n",
        "-4445GI\nT1\n",
        "%KACHI\n",
    );

    /// 先手玉が敵陣へ入城して %KACHI で勝つ合成対局。pair (1,3) = king_entry。
    /// 終端（4 手目適用後、先手手番）は Point27 宣言成立形:
    /// 玉 52（敵陣内）・敵陣内 10 枚（飛 2 + 角 2 + 金 4 + 銀 2）・
    /// 28 点（10 + 大駒加点 4x4 + 持ち歩 2）・王手なし。
    const KACHI_ENTRY_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU33KA43KA63HI73HI21KI31KI41KI51KI61GI71GI00FU00FU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-1918OU\nT1\n",
        "+5352OU\nT1\n",
        "-1817OU\nT1\n",
        "%KACHI\n",
    );

    /// replay は完走するが、終端局面で Point27 宣言が成立しない %KACHI 対局
    /// （宣言失敗 = illegal kachi の棋譜を模す。玉のみで点数・枚数が全く足りない）。
    const KACHI_POINT27_FAIL_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-1918OU\nT1\n",
        "+5352OU\nT1\n",
        "-1817OU\nT1\n",
        "%KACHI\n",
    );

    /// %KACHI だが間の相手手が不正（Move::NONE fallback + 打ち切り）で pair を出せない対局。
    const KACHI_BROKEN_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-5556FU\nT1\n",
        "+5352OU\nT1\n",
        "-1817OU\nT1\n",
        "%KACHI\n",
    );

    const TORYO_CSA: &str = "V2.2\nN+B\nN-W\nPI\n+7776FU\nT1\n-3334FU\nT1\n%TORYO\n";

    fn load_game_from(text: &str) -> (Vec<MoveView>, Color) {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "game.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let entry = &index.entries[0];
        let Some(GameOutcomeView::Win(winner)) = entry.outcome else {
            panic!("winner expected");
        };
        let game = source.load_game(&index, entry).expect("load_game");
        (game.moves, winner)
    }

    #[test]
    fn kachi_game_yields_condition_pairs() {
        let (moves, winner) = load_game_from(KACHI_GAINS_CSA);
        assert_eq!(winner, Color::Black);
        let pairs = build_pairs_for_game(&moves, winner, &test_prov("game.csa")).expect("pairs");
        assert_eq!(pairs.len(), 2);

        let p1 = &pairs[0];
        assert_eq!((p1.ply_before, p1.ply_after), (1, 3));
        assert_eq!(p1.conditions, vec![Condition::CheckResolved]);
        assert!(p1.check_before && !p1.check_after);
        assert_eq!((p1.points_before, p1.points_after), (27, 27));

        let p2 = &pairs[1];
        assert_eq!((p2.ply_before, p2.ply_after), (3, 5));
        assert_eq!(p2.conditions, vec![Condition::PointGain, Condition::ZonePieceGain]);
        assert_eq!((p2.points_before, p2.points_after), (27, 28));
        assert_eq!((p2.zone_before, p2.zone_after), (9, 10));
    }

    #[test]
    fn king_entry_is_detected() {
        let (moves, winner) = load_game_from(KACHI_ENTRY_CSA);
        let pairs = build_pairs_for_game(&moves, winner, &test_prov("game.csa")).expect("pairs");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].conditions, vec![Condition::KingEntry]);
        assert!(!pairs[0].king_in_before && pairs[0].king_in_after);
    }

    #[test]
    fn kachi_fixture_terminals_satisfy_point27() {
        // pair 化を通す fixture は、終端局面（宣言側手番）で Point27 宣言が実際に
        // 成立していることを Position で検証しておく（run_build の突き合わせの前提）。
        for text in [KACHI_GAINS_CSA, KACHI_ENTRY_CSA] {
            let (moves, winner) = load_game_from(text);
            let terminal = terminal_position(&moves).expect("terminal");
            assert_eq!(terminal.side_to_move(), winner, "終端は宣言側手番");
            assert_eq!(terminal.declaration_win(EnteringKingRule::Point27), Move::WIN);
        }
    }

    #[test]
    fn point27_fail_fixture_terminal_does_not_declare() {
        let (moves, winner) = load_game_from(KACHI_POINT27_FAIL_CSA);
        let terminal = terminal_position(&moves).expect("terminal");
        assert_eq!(terminal.side_to_move(), winner);
        assert_eq!(terminal.declaration_win(EnteringKingRule::Point27), Move::NONE);
    }

    #[test]
    fn broken_intermediate_move_yields_no_pairs() {
        // 不正な相手手で replay が Move::NONE + 打ち切りになり、pair は 1 件も出ない。
        // 終端局面の復元も不能（最終手が通常手でない）。
        let (moves, winner) = load_game_from(KACHI_BROKEN_CSA);
        assert!(moves.iter().any(|m| !m.mv.is_normal()));
        assert!(terminal_position(&moves).is_err());
        let pairs = build_pairs_for_game(&moves, winner, &test_prov("game.csa")).expect("pairs");
        assert!(pairs.is_empty());
    }

    fn mv(ply: u32, side: Color, normal: bool) -> MoveView {
        MoveView {
            ply,
            side,
            sfen_before: String::new(),
            mv: if normal {
                Move::from_usi("7g7f").expect("usi")
            } else {
                Move::NONE
            },
            kif_label: String::new(),
            annotation: MoveAnnotation::default(),
        }
    }

    #[test]
    fn pair_indices_require_adjacent_plies_and_normal_moves() {
        let b = Color::Black;
        let w = Color::White;
        // 正常な並び: (0,2) と (2,4)。
        let moves = vec![
            mv(1, b, true),
            mv(2, w, true),
            mv(3, b, true),
            mv(4, w, true),
            mv(5, b, true),
        ];
        assert_eq!(adjacent_winner_pair_indices(&moves, b), vec![(0, 2), (2, 4)]);
        // winner が後手なら後手手番の窓 (1,3) だけが対象になる。
        assert_eq!(adjacent_winner_pair_indices(&moves, w), vec![(1, 3)]);

        // ply 欠番（3 の次が 6）: (0,2) のみ。
        let gapped = vec![
            mv(1, b, true),
            mv(2, w, true),
            mv(3, b, true),
            mv(6, w, true),
            mv(7, b, true),
        ];
        assert_eq!(adjacent_winner_pair_indices(&gapped, b), vec![(0, 2)]);

        // 間の相手手が Move::NONE: skip。
        let broken = vec![mv(1, b, true), mv(2, w, false), mv(3, b, true)];
        assert_eq!(adjacent_winner_pair_indices(&broken, b), Vec::new());

        // 前局面の勝者手が Move::NONE: skip。
        let broken_first = vec![mv(1, b, false), mv(2, w, true), mv(3, b, true)];
        assert_eq!(adjacent_winner_pair_indices(&broken_first, b), Vec::new());
    }

    #[test]
    fn run_build_writes_pairs_and_meta() {
        let in_dir = tempfile::tempdir().expect("tempdir");
        // 先頭の gains は csa_client 日時形式のファイル名にして date_key の伝播も確かめる。
        write_csa(in_dir.path(), "20260101_000000_gains.csa", KACHI_GAINS_CSA);
        write_csa(in_dir.path(), "b_entry.csa", KACHI_ENTRY_CSA);
        write_csa(in_dir.path(), "c_toryo.csa", TORYO_CSA);
        write_csa(in_dir.path(), "d_broken.csa", KACHI_BROKEN_CSA);
        write_csa(in_dir.path(), "e_point27_fail.csa", KACHI_POINT27_FAIL_CSA);
        let out_dir = tempfile::tempdir().expect("tempdir");

        run_build(&BuildArgs {
            input: in_dir.path().to_path_buf(),
            out_dir: out_dir.path().to_path_buf(),
        })
        .expect("run_build");

        let pairs_text =
            fs::read_to_string(out_dir.path().join("pairs.jsonl")).expect("pairs.jsonl");
        let pairs: Vec<PairRecord> = pairs_text
            .lines()
            .map(|l| serde_json::from_str(l).expect("pair json"))
            .collect();
        assert_eq!(pairs.len(), 3);
        assert!(pairs.iter().all(|p| p.winner == 'b'));
        assert!(pairs.iter().all(|p| !p.conditions.is_empty()));
        // 出典情報（date_key / 両エンジン名）の生値が各 pair に載る。
        assert!(pairs.iter().all(|p| p.black_engine == "B" && p.white_engine == "W"));
        assert_eq!(pairs[0].date_key, Some(20260101000000), "日時付きファイル名から抽出");
        assert_eq!(pairs[1].date_key, Some(20260101000000));
        assert_eq!(pairs[2].date_key, None, "日時なしファイル名は null");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out_dir.path().join("meta.json")).unwrap())
                .expect("meta json");
        assert_eq!(meta["games_scanned"], 5);
        // %TORYO の対局は kachi に数えない。
        assert_eq!(meta["kachi_games"], 4);
        // broken は replay 未完走（終端復元不能）として除外。
        assert_eq!(meta["games_skipped_broken"], 1);
        // %KACHI 記録でも終端で Point27 宣言が成立しない対局は除外して計上する。
        assert_eq!(meta["games_skipped_point27_mismatch"], 1);
        assert_eq!(meta["games_skipped_winner_mismatch"], 0);
        assert_eq!(meta["games_with_pairs"], 2);
        assert_eq!(meta["pairs_total"], 3);
        assert_eq!(meta["conditions"]["check_resolved"]["pairs"], 1);
        assert_eq!(meta["conditions"]["check_resolved"]["games"], 1);
        assert_eq!(meta["conditions"]["point_gain"]["pairs"], 1);
        assert_eq!(meta["conditions"]["zone_piece_gain"]["pairs"], 1);
        assert_eq!(meta["conditions"]["king_entry"]["pairs"], 1);
        assert_eq!(meta["conditions"]["king_entry"]["games"], 1);
    }

    #[test]
    fn agreement_scores_order_and_tie() {
        assert_eq!(agreement_score(100, 200), 1.0);
        assert_eq!(agreement_score(200, 200), 0.5);
        assert_eq!(agreement_score(200, 100), 0.0);
    }

    #[test]
    fn aggregator_computes_rates_per_condition() {
        let mut agg = PairAggregator::default();
        agg.push("g1.csa", &[Condition::PointGain], 1.0);
        agg.push("g1.csa", &[Condition::PointGain, Condition::KingEntry], 0.0);
        agg.push("g2.csa", &[Condition::CheckResolved], 1.0);
        let (overall, conditions) = agg.finish(0, DEFAULT_SEED);

        assert_eq!(overall.n_pairs, 3);
        assert_eq!(overall.n_games, 2);
        assert!((overall.agreement.unwrap() - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(overall.ci95_lo, None, "bootstrap=0 では CI を出さない");

        let pg = &conditions["point_gain"];
        assert_eq!((pg.n_pairs, pg.n_games), (2, 1));
        assert_eq!(pg.agreement, Some(0.5));
        let ke = &conditions["king_entry"];
        assert_eq!((ke.n_pairs, ke.n_games), (1, 1));
        assert_eq!(ke.agreement, Some(0.0));
        let zp = &conditions["zone_piece_gain"];
        assert_eq!((zp.n_pairs, zp.n_games), (0, 0));
        assert_eq!(zp.agreement, None);
    }

    #[test]
    fn percentile_uses_round_of_n_minus_one_times_q() {
        // index = round((n-1) * q) 方式の境界順位を直接固定する。
        let sorted: Vec<f64> = (0..10_000).map(f64::from).collect();
        // (10000-1) * 0.025 = 249.975 → round = 250（nearest-rank の ceil(n*q)-1 = 249 とは別）。
        assert_eq!(percentile_sorted(&sorted, 0.025), 250.0);
        // (10000-1) * 0.975 = 9749.025 → round = 9749。
        assert_eq!(percentile_sorted(&sorted, 0.975), 9749.0);
        // 端の q は端の要素。
        assert_eq!(percentile_sorted(&sorted, 0.0), 0.0);
        assert_eq!(percentile_sorted(&sorted, 1.0), 9999.0);
        // 単一要素は q に依らずその値。
        assert_eq!(percentile_sorted(&[0.5], 0.025), 0.5);
        assert_eq!(percentile_sorted(&[0.5], 0.975), 0.5);
    }

    #[test]
    fn slot_seeds_do_not_collide_across_adjacent_seeds() {
        // 旧方式 `seed + slot` は (seed=42, slot=1) と (seed=43, slot=0) が同一ストリーム
        // になっていた。大定数乗算の xor 派生で近傍 seed 間の衝突が無いことを固定する。
        for seed in [0u64, 42, DEFAULT_SEED] {
            for a in 0..SLOTS {
                for b in 0..SLOTS {
                    if a != b {
                        assert_ne!(slot_seed(seed, a), slot_seed(seed, b));
                    }
                    assert_ne!(slot_seed(seed, a), slot_seed(seed + 1, b));
                }
            }
        }
        // slot 0（全体）は seed をそのまま使う。
        assert_eq!(slot_seed(DEFAULT_SEED, 0), DEFAULT_SEED);
    }

    #[test]
    fn bootstrap_is_deterministic_for_same_seed() {
        let per_game = vec![
            GameAgg { sum: 3.0, count: 4 },
            GameAgg { sum: 1.0, count: 2 },
            GameAgg { sum: 0.5, count: 1 },
            GameAgg { sum: 5.0, count: 6 },
        ];
        let a = bootstrap_ci95(&per_game, 1000, DEFAULT_SEED).expect("ci");
        let b = bootstrap_ci95(&per_game, 1000, DEFAULT_SEED).expect("ci");
        assert_eq!(a, b, "同 seed 同入力で CI は bit 一致する");
        assert!(a.0 <= a.1);
        assert!((0.0..=1.0).contains(&a.0) && (0.0..=1.0).contains(&a.1));
    }

    #[test]
    fn bootstrap_single_game_collapses_to_point() {
        let per_game = vec![GameAgg { sum: 3.0, count: 4 }];
        let (lo, hi) = bootstrap_ci95(&per_game, 100, DEFAULT_SEED).expect("ci");
        assert_eq!(lo, 0.75);
        assert_eq!(hi, 0.75);
    }

    #[test]
    fn aggregator_ci_is_deterministic_end_to_end() {
        let build = || {
            let mut agg = PairAggregator::default();
            agg.push("g1.csa", &[Condition::PointGain], 1.0);
            agg.push("g2.csa", &[Condition::PointGain], 0.0);
            agg.push("g3.csa", &[Condition::PointGain], 0.5);
            agg.finish(500, DEFAULT_SEED)
        };
        let (o1, c1) = build();
        let (o2, c2) = build();
        assert_eq!(o1, o2);
        assert_eq!(c1, c2);
        assert!(c1["point_gain"].ci95_lo.is_some());
    }
}
