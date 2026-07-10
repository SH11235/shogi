//! 入玉評価テストセットの構築・採点ツール。
//!
//! CSA replay と勝敗導出は `replay::csa_source::CsaSource` に委譲する。宣言判定は
//! `Position::declaration_win(EnteringKingRule::Point27)` を使う。core が
//! `entering_king_point_info` を公開していないため、点数系フィールドは JSONL に出さない。

use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::mem::size_of;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use rshogi_core::nnue::{
    AccumulatorStackVariant, LayerStackBucketMode, LayerStacksAccCache,
    SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, evaluate_dispatch, get_network, init_nnue,
    set_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, EnteringKingRule, Move, Rank};
use serde::{Deserialize, Serialize};

use crate::replay::csa_source::CsaSource;
use crate::replay::model::{GameIndex, GameIndexEntry, GameOutcomeView, GameSource, MoveView};

const DEFAULT_MIN_PLY_FROM_ENTRY: i32 = -20;
const DEFAULT_SAMPLE_STRIDE: u32 = 4;
const DEFAULT_SCALE: f64 = 290.0;
const DECISIVE_CP: i32 = 600;
const PROB_EPS: f64 = 1e-12;

#[derive(Parser, Debug)]
#[command(
    name = "ek_testset",
    version,
    about = "入玉評価テストセットを CSA から構築し、NNUE 静的評価で採点する"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// held-out CSA から入玉評価テストセットを構築する。
    Build(BuildArgs),
    /// testset.jsonl を native NNUE 評価で採点する。
    Eval(EvalArgs),
}

#[derive(Parser, Debug)]
struct BuildArgs {
    /// 入力 CSA ファイルまたは CSA ディレクトリ。
    #[arg(long)]
    input: PathBuf,
    /// 出力ディレクトリ。
    #[arg(long)]
    out_dir: PathBuf,
    /// 玉の敵陣初侵入 ply から何手前以降を対象にするか。
    #[arg(long, default_value_t = DEFAULT_MIN_PLY_FROM_ENTRY, allow_hyphen_values = true)]
    min_ply_from_entry: i32,
    /// 対象区間を何手ごとにサンプルするか。
    #[arg(long, default_value_t = DEFAULT_SAMPLE_STRIDE)]
    sample_stride: u32,
    /// draw を除外するか (`--drop-draw false` で draw 対局も含める)。
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    drop_draw: bool,
}

#[derive(Parser, Debug)]
struct EvalArgs {
    /// `ek_testset build` が出した testset.jsonl。
    #[arg(long)]
    testset: PathBuf,
    /// NNUE ファイル。
    #[arg(long)]
    eval_file: PathBuf,
    /// LayerStacks progress8kpabs 用 progress.bin。
    #[arg(long)]
    progress_file: PathBuf,
    /// sigmoid(eval / scale) の scale。
    #[arg(long, default_value_t = DEFAULT_SCALE)]
    scale: f64,
    /// metrics.json の出力先。
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct TestsetRecord {
    sfen: String,
    stm: char,
    ply: u32,
    source_csa: String,
    is_declarable: bool,
    dt_label: Option<Label>,
    oc_label: Label,
    #[serde(skip_serializing_if = "Option::is_none")]
    floodgate_eval_cp: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Label {
    Win,
    Loss,
    Draw,
}

#[derive(Debug, Serialize)]
struct BuildMeta {
    input: String,
    min_ply_from_entry: i32,
    sample_stride: u32,
    drop_draw: bool,
    games_indexed: usize,
    games_used: usize,
    games_skipped_broken: usize,
    records: usize,
    dt_records: usize,
    oc_records: usize,
    sources: Vec<String>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EvalMetrics {
    testset: String,
    eval_file: String,
    progress_file: String,
    scale: f64,
    records: usize,
    dt: DtMetrics,
    oc: OcMetrics,
}

#[derive(Debug, Serialize, Default, PartialEq)]
struct DtMetrics {
    n: usize,
    sign_acc: Option<f64>,
    decisive_acc: Option<f64>,
    eval_median: Option<i32>,
    eval_p10: Option<i32>,
}

#[derive(Debug, Serialize, Default, PartialEq)]
struct OcMetrics {
    n: usize,
    sign_acc: Option<f64>,
    wdl_cross_entropy: Option<f64>,
    brier: Option<f64>,
    calibration: Vec<CalibrationBin>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct CalibrationBin {
    bin: usize,
    n: usize,
    avg_pred: f64,
    win_rate: f64,
}

#[derive(Debug, Clone, Copy)]
struct ScoredLabel {
    eval_cp: i32,
    label: Label,
}

/// OC calibration の固定幅ビン数（予測勝率 [0,1] を等幅分割）。
///
/// 全予測を保持して分位分割する代わりに、ビンごとの件数・予測和・勝敗和だけを
/// 逐次加算する。ピークメモリを入力件数に非依存（ビン数固定）にするため。
const OC_CALIBRATION_BINS: usize = 10;

/// DT 評価値ヒストグラムの片側範囲 [cp]。
///
/// 全評価値を保持する代わりに 1cp 幅の固定長ヒストグラムへ逐次加算し、ピークメモリを
/// 入力件数に非依存にする。範囲は静的評価の実用域を十分覆い、範囲外は端に飽和させる
/// （分位値は範囲内なら正確、範囲外は ±DT_EVAL_HIST_MAX_CP に丸まる）。
const DT_EVAL_HIST_MAX_CP: i32 = 32_000;
const DT_EVAL_HIST_LEN: usize = DT_EVAL_HIST_MAX_CP as usize * 2 + 1;

#[derive(Debug)]
struct EvalMetricBuilder {
    records: usize,
    // index = clamp(eval_cp) + DT_EVAL_HIST_MAX_CP の件数。
    dt_hist: Box<[u64]>,
    oc_n: usize,
    oc_sign_ok: usize,
    oc_ce: f64,
    oc_brier: f64,
    // 等幅ビンごとの逐次集計（件数 / 予測勝率和 / 勝敗和）。
    oc_cal_count: [usize; OC_CALIBRATION_BINS],
    oc_cal_pred_sum: [f64; OC_CALIBRATION_BINS],
    oc_cal_win_sum: [f64; OC_CALIBRATION_BINS],
}

impl Default for EvalMetricBuilder {
    fn default() -> Self {
        Self {
            records: 0,
            dt_hist: vec![0; DT_EVAL_HIST_LEN].into_boxed_slice(),
            oc_n: 0,
            oc_sign_ok: 0,
            oc_ce: 0.0,
            oc_brier: 0.0,
            oc_cal_count: [0; OC_CALIBRATION_BINS],
            oc_cal_pred_sum: [0.0; OC_CALIBRATION_BINS],
            oc_cal_win_sum: [0.0; OC_CALIBRATION_BINS],
        }
    }
}

#[derive(Debug, Clone)]
struct PositionSample {
    sfen: String,
    ply: u32,
    floodgate_eval_cp: Option<i32>,
}

/// CLI entrypoint。
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build(args) => run_build(&args),
        Command::Eval(args) => run_eval(&args),
    }
}

fn run_build(args: &BuildArgs) -> Result<()> {
    if args.sample_stride == 0 {
        bail!("--sample-stride は 1 以上を指定してください");
    }

    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("出力ディレクトリを作成できません: {}", args.out_dir.display()))?;

    let source = CsaSource::new(&args.input);
    let index = source.build_index()?;
    for warning in &index.warnings {
        eprintln!("warning: {warning}");
    }

    let testset_path = args.out_dir.join("testset.jsonl");
    let sfens_path = args.out_dir.join("sfens.txt");
    let meta_path = args.out_dir.join("meta.json");
    let mut testset = BufWriter::new(File::create(&testset_path)?);
    let mut sfens = BufWriter::new(File::create(&sfens_path)?);

    let mut records = 0usize;
    let mut dt_records = 0usize;
    let mut oc_records = 0usize;
    let mut games_used = 0usize;
    let mut games_skipped_broken = 0usize;

    for entry in &index.entries {
        let Some(outcome) = entry.outcome else {
            continue;
        };
        if args.drop_draw && matches!(outcome, GameOutcomeView::Draw) {
            continue;
        }

        let game = source.load_game(&index, entry)?;
        let source_csa = source_path(&index, entry)?;
        if !replay_is_complete(&game.moves, entry.ply_count) {
            games_skipped_broken += 1;
            eprintln!("warning: {source_csa}: 再生を末尾まで信頼できないため対局ごと除外します");
            continue;
        }
        let built = build_records_for_game(
            &game.moves,
            outcome,
            &source_csa,
            args.min_ply_from_entry,
            args.sample_stride,
        )?;
        if !built.is_empty() {
            games_used += 1;
        }
        for record in built {
            if record.dt_label == Some(Label::Win) {
                dt_records += 1;
            }
            // eval 側の OC 採点は draw を除外するため、meta の oc_records も win/loss のみ数える。
            if matches!(record.oc_label, Label::Win | Label::Loss) {
                oc_records += 1;
            }
            serde_json::to_writer(&mut testset, &record)?;
            writeln!(testset)?;
            writeln!(sfens, "{}", record.sfen)?;
            records += 1;
        }
    }
    testset.flush()?;
    sfens.flush()?;

    let meta = BuildMeta {
        input: args.input.display().to_string(),
        min_ply_from_entry: args.min_ply_from_entry,
        sample_stride: args.sample_stride,
        drop_draw: args.drop_draw,
        games_indexed: index.entries.len(),
        games_used,
        games_skipped_broken,
        records,
        dt_records,
        oc_records,
        sources: index.pair_files.iter().map(|m| m.path.display().to_string()).collect(),
        notes: vec![
            "core が entering_king_point_info を公開していないため points_stm / king_in_enemy_stm / enemy_zone_pieces_stm は省略".to_string(),
        ],
    };
    let mut meta_writer = BufWriter::new(File::create(&meta_path)?);
    serde_json::to_writer_pretty(&mut meta_writer, &meta)?;
    writeln!(meta_writer)?;
    meta_writer.flush()?;

    eprintln!(
        "wrote {} records (dt={}, oc={}) to {}",
        records,
        dt_records,
        oc_records,
        testset_path.display()
    );
    Ok(())
}

fn build_records_for_game(
    moves: &[MoveView],
    outcome: GameOutcomeView,
    source_csa: &str,
    min_ply_from_entry: i32,
    sample_stride: u32,
) -> Result<Vec<TestsetRecord>> {
    let samples = position_samples(moves)?;
    let first_entry_ply = first_enemy_entry_ply(&samples)?;
    let Some(first_entry_ply) = first_entry_ply else {
        return Ok(Vec::new());
    };
    let threshold = i64::from(first_entry_ply) + i64::from(min_ply_from_entry);
    let mut records = Vec::new();

    for sample in samples {
        let ply = i64::from(sample.ply);
        if ply < threshold {
            continue;
        }
        if (ply - threshold).rem_euclid(i64::from(sample_stride)) != 0 {
            continue;
        }

        let mut pos = Position::new();
        pos.set_sfen(&sample.sfen)
            .with_context(|| format!("SFEN を復元できません: {}", sample.sfen))?;
        let stm = pos.side_to_move();
        let is_declarable = pos.declaration_win(EnteringKingRule::Point27) != Move::NONE;
        let oc_label = oc_label_for_stm(outcome, stm);

        records.push(TestsetRecord {
            sfen: sample.sfen,
            stm: color_label(stm),
            ply: sample.ply,
            source_csa: source_csa.to_string(),
            is_declarable,
            dt_label: is_declarable.then_some(Label::Win),
            oc_label,
            floodgate_eval_cp: sample.floodgate_eval_cp,
        });
    }

    Ok(records)
}

fn position_samples(moves: &[MoveView]) -> Result<Vec<PositionSample>> {
    let mut samples = Vec::with_capacity(moves.len() + 1);
    for mv in moves {
        samples.push(PositionSample {
            sfen: mv.sfen_before.clone(),
            ply: mv.ply,
            floodgate_eval_cp: mv.annotation.score_cp,
        });
    }

    if let Some(last) = moves.last()
        && last.mv.is_normal()
    {
        let mut pos = Position::new();
        pos.set_sfen(&last.sfen_before)
            .with_context(|| format!("SFEN を復元できません: {}", last.sfen_before))?;
        let gives_check = pos.gives_check(last.mv);
        pos.do_move(last.mv, gives_check);
        samples.push(PositionSample {
            sfen: pos.to_sfen(),
            ply: last.ply.saturating_add(1),
            floodgate_eval_cp: None,
        });
    }

    Ok(samples)
}

fn first_enemy_entry_ply(samples: &[PositionSample]) -> Result<Option<u32>> {
    let mut first = None;
    for sample in samples {
        let mut pos = Position::new();
        pos.set_sfen(&sample.sfen)
            .with_context(|| format!("SFEN を復元できません: {}", sample.sfen))?;
        if king_in_enemy_zone(&pos, Color::Black) || king_in_enemy_zone(&pos, Color::White) {
            first = Some(sample.ply);
            break;
        }
    }
    Ok(first)
}

fn king_in_enemy_zone(pos: &Position, side: Color) -> bool {
    match side {
        Color::Black => {
            matches!(pos.king_square(side).rank(), Rank::Rank1 | Rank::Rank2 | Rank::Rank3)
        }
        Color::White => {
            matches!(pos.king_square(side).rank(), Rank::Rank7 | Rank::Rank8 | Rank::Rank9)
        }
    }
}

/// 再生が末尾まで信頼できるか。
///
/// `CsaSource::load_game` は core 上で信頼できない手を `Move::NONE` にして再生を打ち切る。
/// 打ち切られた対局の途中局面へ最終結果ラベルを付けると誤ラベルになるため、対局ごと除外する。
fn replay_is_complete(moves: &[MoveView], expected_normal_moves: u32) -> bool {
    moves.len() as u64 == u64::from(expected_normal_moves) && moves.iter().all(|m| m.mv.is_normal())
}

fn oc_label_for_stm(outcome: GameOutcomeView, stm: Color) -> Label {
    match outcome {
        GameOutcomeView::Win(winner) if winner == stm => Label::Win,
        GameOutcomeView::Win(_) => Label::Loss,
        GameOutcomeView::Draw => Label::Draw,
    }
}

fn source_path(index: &GameIndex, entry: &GameIndexEntry) -> Result<String> {
    let crate::replay::model::GameSourceRef::Csa { file_idx, .. } = entry.source else {
        bail!("CSA 以外の GameIndexEntry が渡されました");
    };
    let meta = index
        .pair_file(file_idx)
        .ok_or_else(|| anyhow!("file_idx {file_idx} が index にありません"))?;
    Ok(meta.path.display().to_string())
}

fn color_label(c: Color) -> char {
    match c {
        Color::Black => 'b',
        Color::White => 'w',
    }
}

fn run_eval(args: &EvalArgs) -> Result<()> {
    if !args.scale.is_finite() || args.scale <= 0.0 {
        bail!("--scale は正の有限値を指定してください");
    }

    let weights = load_progress_coeff_kpabs(&args.progress_file)
        .map_err(|e| anyhow!("progress 読み込みに失敗しました: {e}"))?;
    set_layer_stack_progress_kpabs_weights(weights)
        .map_err(|e| anyhow!("progress 設定に失敗しました: {e}"))?;
    set_layer_stack_bucket_mode(LayerStackBucketMode::Progress8KPAbs);
    init_nnue(&args.eval_file)
        .with_context(|| format!("NNUE を読み込めません: {}", args.eval_file.display()))?;

    let network = get_network().ok_or_else(|| anyhow!("NNUE が初期化されていません"))?;
    if !network.is_layer_stacks() {
        bail!(
            "ek_testset eval は LayerStacks NNUE のみ対応しています: {}",
            network.architecture_name()
        );
    }
    let mut stack = AccumulatorStackVariant::from_network(&network);
    let mut acc_cache: Option<LayerStacksAccCache> =
        Some(network.as_layer_stacks().new_acc_cache());

    let mut builder = EvalMetricBuilder::default();
    let file = File::open(&args.testset)
        .with_context(|| format!("testset を開けません: {}", args.testset.display()))?;
    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: TestsetRecord = serde_json::from_str(&line).with_context(|| {
            format!("{}:{}: JSON を読めません", args.testset.display(), line_no + 1)
        })?;
        let mut pos = Position::new();
        pos.set_sfen(&record.sfen).with_context(|| {
            format!("{}:{}: SFEN を読めません", args.testset.display(), line_no + 1)
        })?;
        stack.reset();
        let eval_cp = evaluate_dispatch(&pos, &mut stack, &mut acc_cache).raw();
        builder.push(&record, eval_cp, args.scale);
    }

    let (records, dt, oc) = builder.finish();
    let out = EvalMetrics {
        testset: args.testset.display().to_string(),
        eval_file: args.eval_file.display().to_string(),
        progress_file: args.progress_file.display().to_string(),
        scale: args.scale,
        records,
        dt,
        oc,
    };

    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    serde_json::to_writer_pretty(&mut locked, &out)?;
    writeln!(locked)?;

    if let Some(path) = &args.out {
        write_json_pretty(path, &out)?;
    }
    Ok(())
}

fn load_progress_coeff_kpabs(path: &Path) -> Result<Box<[f32]>, String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
    let expected = SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * size_of::<f64>();
    if bytes.len() != expected {
        return Err(format!(
            "progress.bin size mismatch: got {} bytes, expected {}",
            bytes.len(),
            expected
        ));
    }
    let weights: Vec<f32> = bytes
        .chunks_exact(size_of::<f64>())
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("chunk size is checked")) as f32)
        .collect();
    Ok(weights.into_boxed_slice())
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
fn compute_metrics(records: &[(TestsetRecord, i32)], scale: f64) -> (DtMetrics, OcMetrics) {
    let mut builder = EvalMetricBuilder::default();
    for (record, eval_cp) in records {
        builder.push(record, *eval_cp, scale);
    }
    let (_, dt, oc) = builder.finish();
    (dt, oc)
}

impl EvalMetricBuilder {
    fn push(&mut self, record: &TestsetRecord, eval_cp: i32, scale: f64) {
        self.records += 1;
        if record.dt_label == Some(Label::Win) {
            let clamped = eval_cp.clamp(-DT_EVAL_HIST_MAX_CP, DT_EVAL_HIST_MAX_CP);
            self.dt_hist[(clamped + DT_EVAL_HIST_MAX_CP) as usize] += 1;
        }
        if matches!(record.oc_label, Label::Win | Label::Loss) {
            self.push_oc(
                ScoredLabel {
                    eval_cp,
                    label: record.oc_label,
                },
                scale,
            );
        }
    }

    fn push_oc(&mut self, record: ScoredLabel, scale: f64) {
        let target = match record.label {
            Label::Win => 1.0,
            Label::Loss => 0.0,
            Label::Draw => unreachable!("draw は OC 採点対象外"),
        };
        let p = sigmoid(f64::from(record.eval_cp) / scale).clamp(PROB_EPS, 1.0 - PROB_EPS);
        if (record.eval_cp > 0 && record.label == Label::Win)
            || (record.eval_cp < 0 && record.label == Label::Loss)
        {
            self.oc_sign_ok += 1;
        }
        self.oc_n += 1;
        self.oc_ce += -(target * p.ln() + (1.0 - target) * (1.0 - p).ln());
        self.oc_brier += (p - target).powi(2);
        // 予測勝率 p を等幅ビンへ振り分けて逐次加算（[0,1) を BINS 等分、p=1.0 は最終ビン）。
        let bin = ((p * OC_CALIBRATION_BINS as f64) as usize).min(OC_CALIBRATION_BINS - 1);
        self.oc_cal_count[bin] += 1;
        self.oc_cal_pred_sum[bin] += p;
        self.oc_cal_win_sum[bin] += target;
    }

    fn finish(self) -> (usize, DtMetrics, OcMetrics) {
        let oc = if self.oc_n == 0 {
            OcMetrics::default()
        } else {
            OcMetrics {
                n: self.oc_n,
                sign_acc: Some(rate(self.oc_sign_ok, self.oc_n)),
                wdl_cross_entropy: Some(self.oc_ce / self.oc_n as f64),
                brier: Some(self.oc_brier / self.oc_n as f64),
                calibration: self.calibration_bins(),
            }
        };
        (self.records, dt_metrics(&self.dt_hist), oc)
    }

    /// 等幅ビンの逐次集計から calibration テーブルを構築する（空ビンは省く）。
    fn calibration_bins(&self) -> Vec<CalibrationBin> {
        let mut out = Vec::new();
        for bin in 0..OC_CALIBRATION_BINS {
            let n = self.oc_cal_count[bin];
            if n == 0 {
                continue;
            }
            out.push(CalibrationBin {
                bin,
                n,
                avg_pred: self.oc_cal_pred_sum[bin] / n as f64,
                win_rate: self.oc_cal_win_sum[bin] / n as f64,
            });
        }
        out
    }
}

fn dt_metrics(hist: &[u64]) -> DtMetrics {
    let n: u64 = hist.iter().sum();
    if n == 0 {
        return DtMetrics::default();
    }
    // cp > threshold の件数（threshold は DT_EVAL_HIST_MAX_CP 未満が前提）。
    let count_above = |threshold: i32| -> u64 {
        hist[(threshold + 1 + DT_EVAL_HIST_MAX_CP) as usize..].iter().sum()
    };
    DtMetrics {
        n: n as usize,
        sign_acc: Some(count_above(0) as f64 / n as f64),
        decisive_acc: Some(count_above(DECISIVE_CP) as f64 / n as f64),
        eval_median: Some(hist_percentile(hist, n, 0.5)),
        eval_p10: Some(hist_percentile(hist, n, 0.1)),
    }
}

/// ソート列の `floor((n-1)*q)` 番目に相当する値をヒストグラムの累積和で求める。
fn hist_percentile(hist: &[u64], n: u64, q: f64) -> i32 {
    debug_assert!(n > 0);
    let target = ((n - 1) as f64 * q).floor() as u64;
    let mut cum = 0u64;
    for (idx, &count) in hist.iter().enumerate() {
        cum += count;
        if cum > target {
            return idx as i32 - DT_EVAL_HIST_MAX_CP;
        }
    }
    unreachable!("target < n のため累積和は必ず target を超える")
}

fn rate(num: usize, den: usize) -> f64 {
    num as f64 / den as f64
}

fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_csa(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).expect("create");
        f.write_all(text.as_bytes()).expect("write");
        path
    }

    fn build_records_from_csa(text: &str) -> Vec<TestsetRecord> {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "game.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let entry = &index.entries[0];
        let outcome = entry.outcome.expect("outcome");
        let game = source.load_game(&index, entry).expect("load_game");
        build_records_for_game(&game.moves, outcome, "game.csa", -20, 1).expect("records")
    }

    const DECLARABLE_CSA: &str = concat!(
        "V2.2\n",
        "P+51OU11HI21KA31KI41KI61GI71GI81KE91KE12KY22KY00HI00FU00FU00FU00FU00FU00FU00FU00FU00FU\n",
        "P-59OU00FU\n",
        "+\n",
        "'* 1200\n",
        "+0044FU\n",
        "'* -30\n",
        "-0056FU\n",
        "%KACHI\n",
    );

    const ENTERED_RESIGN_CSA: &str =
        concat!("V2.2\n", "P+51OU00FU\n", "P-59OU\n", "+\n", "'* -80\n", "+0044FU\n", "%TORYO\n",);

    #[test]
    fn drop_draw_flag_is_settable() {
        let base = ["ek_testset", "build", "--input", "in", "--out-dir", "out"];
        let cli = Cli::try_parse_from(base).expect("parse default");
        let Command::Build(args) = cli.command else {
            panic!("build expected")
        };
        assert!(args.drop_draw);

        let with_false = base.iter().chain(&["--drop-draw", "false"]);
        let cli = Cli::try_parse_from(with_false).expect("parse --drop-draw false");
        let Command::Build(args) = cli.command else {
            panic!("build expected")
        };
        assert!(!args.drop_draw);
    }

    #[test]
    fn build_labels_declarable_csa() {
        let records = build_records_from_csa(DECLARABLE_CSA);
        assert!(!records.is_empty());
        let terminal = records.iter().find(|r| r.ply == 3).expect("terminal sample");
        assert_eq!(terminal.stm, 'b');
        assert!(terminal.is_declarable);
        assert_eq!(terminal.dt_label, Some(Label::Win));
        assert_eq!(terminal.oc_label, Label::Win);
        assert_eq!(terminal.floodgate_eval_cp, None);
    }

    #[test]
    fn build_keeps_side_relative_floodgate_eval_for_white_stm() {
        let records = build_records_from_csa(DECLARABLE_CSA);
        let white = records.iter().find(|r| r.stm == 'w').expect("white sample");
        assert_eq!(white.floodgate_eval_cp, Some(30));
    }

    #[test]
    fn build_labels_entered_but_not_declarable_resign_csa() {
        let records = build_records_from_csa(ENTERED_RESIGN_CSA);
        assert!(!records.is_empty());
        let r = &records[0];
        assert_eq!(r.stm, 'b');
        assert!(!r.is_declarable);
        assert_eq!(r.dt_label, None);
        assert_eq!(r.oc_label, Label::Win);
        assert_eq!(r.floodgate_eval_cp, Some(-80));
    }

    // 打歩は 1 段目に打てないため、core の合法手ゲートで `Move::NONE` にフォールバックする。
    const ILLEGAL_MOVE_CSA: &str =
        concat!("V2.2\n", "P+51OU00FU\n", "P-59OU\n", "+\n", "+0011FU\n", "%TORYO\n",);

    fn load_first_game(text: &str) -> (Vec<MoveView>, u32) {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "game.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let entry = &index.entries[0];
        let game = source.load_game(&index, entry).expect("load_game");
        (game.moves, entry.ply_count)
    }

    #[test]
    fn replay_is_complete_accepts_trusted_and_rejects_illegal_replay() {
        let (moves, ply_count) = load_first_game(DECLARABLE_CSA);
        assert!(replay_is_complete(&moves, ply_count));

        let (moves, ply_count) = load_first_game(ILLEGAL_MOVE_CSA);
        assert!(!replay_is_complete(&moves, ply_count));
    }

    #[test]
    fn dt_metrics_from_histogram_match_sorted_semantics() {
        let dt = |eval_cp: i32| {
            let mut r = TestsetRecord {
                sfen: String::new(),
                stm: 'b',
                ply: 1,
                source_csa: String::new(),
                is_declarable: true,
                dt_label: Some(Label::Win),
                oc_label: Label::Win,
                floodgate_eval_cp: None,
            };
            r.sfen = "4k4/9/9/9/9/9/9/9/4K4 b - 1".to_string();
            (r, eval_cp)
        };
        // -40000 は端 (-32000) に飽和する。ソート列は [-32000, 0, 100, 700]。
        let records = vec![dt(700), dt(-40_000), dt(0), dt(100)];
        let (dt, _) = compute_metrics(&records, 100.0);
        assert_eq!(dt.n, 4);
        assert_eq!(dt.sign_acc, Some(0.5));
        assert_eq!(dt.decisive_acc, Some(0.25));
        assert_eq!(dt.eval_median, Some(0));
        assert_eq!(dt.eval_p10, Some(-32_000));
    }

    #[test]
    fn eval_metrics_are_checked_from_synthetic_values() {
        let base = TestsetRecord {
            sfen: "4k4/9/9/9/9/9/9/9/4K4 b - 1".to_string(),
            stm: 'b',
            ply: 1,
            source_csa: "x.csa".to_string(),
            is_declarable: false,
            dt_label: None,
            oc_label: Label::Win,
            floodgate_eval_cp: None,
        };
        let mut dt_win = base.clone();
        dt_win.is_declarable = true;
        dt_win.dt_label = Some(Label::Win);
        let mut loss = base.clone();
        loss.oc_label = Label::Loss;
        let records = vec![(dt_win, 700), (base, 100), (loss, -100)];
        let (dt, oc) = compute_metrics(&records, 100.0);
        assert_eq!(dt.n, 1);
        assert_eq!(dt.sign_acc, Some(1.0));
        assert_eq!(dt.decisive_acc, Some(1.0));
        assert_eq!(dt.eval_median, Some(700));
        assert_eq!(oc.n, 3);
        assert_eq!(oc.sign_acc, Some(1.0));
        let expected_p = sigmoid(1.0);
        let expected_ce = -((expected_p.ln() * 2.0) + sigmoid(7.0).ln()) / 3.0;
        assert!((oc.wdl_cross_entropy.unwrap() - expected_ce).abs() < 1e-12);
        assert_eq!(oc.calibration.iter().map(|b| b.n).sum::<usize>(), 3);
    }
}
