//! nnue_saturation - LayerStacks NNUE の活性飽和率を実局面で計測する診断ツール。
//!
//! ClippedReLU / SqrClippedReLU 系の活性は u8 [0,127] に clamp されるため、
//! 127 到達率が高いほど量子化天井で情報が落ちている（評価値インフレの副作用の計器）。
//! FT 出力 / L1→L2 / L2→output の 3 段を bucket 別に集計する。
//!
//! 重み側の i8/i16 飽和は rshogi-nnue (tatara) の
//! `crates/nnue-format/examples/clamp_stats.rs` が担当する（export 時量子化の話のため）。

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;

use rshogi_core::nnue::{
    AccumulatorLayerStacks, LayerStackBucketMode, LsFeatureSpec, LsSaturationCounts, NNUENetwork,
    NetworkLayerStacks, compute_layer_stack_progress8kpabs_bucket_index,
    get_layer_stack_progress_kpabs_weights, load_progress_coeff_kpabs, ls_dispatch_ft_size,
    set_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
    sqr_clipped_relu_transform,
};
use rshogi_core::position::Position;
use rshogi_core::types::Color;

#[derive(Parser)]
#[command(
    name = "nnue_saturation",
    about = "LayerStacks NNUE の活性飽和率を実局面で計測"
)]
struct Cli {
    /// NNUE ファイルパス。
    #[arg(long)]
    nnue: PathBuf,

    /// SFEN ファイル（1 行 1 局面）。
    #[arg(long)]
    sfens: PathBuf,

    /// progress.bin (progress8kpabs 進行度重み) のパス。
    #[arg(long)]
    progress_coeff: PathBuf,

    /// 読む局面数の上限（省略時は全件）。
    #[arg(long)]
    count: Option<usize>,

    /// 集計 JSON の出力先（省略時は標準出力のみ）。
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct SaturationReport {
    nnue: String,
    sfens: String,
    positions: u64,
    total: StageRates,
    per_bucket: Vec<BucketReport>,
}

#[derive(Debug, Serialize)]
struct BucketReport {
    bucket: usize,
    positions: u64,
    #[serde(flatten)]
    rates: StageRates,
}

/// 活性 3 段の飽和率（127 到達数 / 総数）。
#[derive(Debug, Serialize)]
struct StageRates {
    ft_sat: u64,
    ft_total: u64,
    ft_rate: f64,
    l1_act_sat: u64,
    l1_act_total: u64,
    l1_act_rate: f64,
    l2_act_sat: u64,
    l2_act_total: u64,
    l2_act_rate: f64,
}

impl StageRates {
    fn from_counts(c: &LsSaturationCounts) -> Self {
        let rate = |sat: u64, total: u64| {
            if total == 0 {
                0.0
            } else {
                sat as f64 / total as f64
            }
        };
        Self {
            ft_sat: c.ft_sat,
            ft_total: c.ft_total,
            ft_rate: rate(c.ft_sat, c.ft_total),
            l1_act_sat: c.l1_act_sat,
            l1_act_total: c.l1_act_total,
            l1_act_rate: rate(c.l1_act_sat, c.l1_act_total),
            l2_act_sat: c.l2_act_sat,
            l2_act_total: c.l2_act_total,
            l2_act_rate: rate(c.l2_act_sat, c.l2_act_total),
        }
    }
}

fn merge(acc: &mut LsSaturationCounts, c: &LsSaturationCounts) {
    acc.ft_sat += c.ft_sat;
    acc.ft_total += c.ft_total;
    acc.l1_act_sat += c.l1_act_sat;
    acc.l1_act_total += c.l1_act_total;
    acc.l2_act_sat += c.l2_act_sat;
    acc.l2_act_total += c.l2_act_total;
}

fn run_for_network<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
    FT: LsFeatureSpec,
>(
    cli: &Cli,
    network: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT, FT>,
) -> Result<()> {
    let file = std::fs::File::open(&cli.sfens)
        .with_context(|| format!("sfens を開けません: {:?}", cli.sfens))?;
    let reader = BufReader::new(file);

    let mut pos = Position::new();
    let mut acc = AccumulatorLayerStacks::<L1>::new();
    let mut bucket_counts = vec![LsSaturationCounts::default(); network.num_buckets];
    let mut bucket_positions = vec![0u64; network.num_buckets];
    let limit = cli.count.unwrap_or(usize::MAX);

    for line in reader.lines().take(limit) {
        let sfen = line?;
        let sfen = sfen.trim();
        if sfen.is_empty() {
            continue;
        }
        pos.set_sfen(sfen).with_context(|| format!("SFEN を読めません: {sfen}"))?;
        network.refresh_accumulator(&pos, &mut acc);

        let side_to_move = pos.side_to_move();
        let (us_acc, them_acc) = if side_to_move == Color::Black {
            (acc.get(Color::Black as usize), acc.get(Color::White as usize))
        } else {
            (acc.get(Color::White as usize), acc.get(Color::Black as usize))
        };
        let mut transformed = [0u8; L1];
        sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed);

        let bucket_index = compute_layer_stack_progress8kpabs_bucket_index(
            &pos,
            side_to_move,
            get_layer_stack_progress_kpabs_weights(),
            network.num_buckets,
        );
        network.layer_stacks.buckets[bucket_index]
            .propagate_counting_saturation(&transformed, &mut bucket_counts[bucket_index]);
        bucket_positions[bucket_index] += 1;
    }

    let mut total = LsSaturationCounts::default();
    for c in &bucket_counts {
        merge(&mut total, c);
    }
    let report = SaturationReport {
        nnue: cli.nnue.display().to_string(),
        sfens: cli.sfens.display().to_string(),
        positions: bucket_positions.iter().sum(),
        total: StageRates::from_counts(&total),
        per_bucket: bucket_counts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.ft_total > 0)
            .map(|(bucket, c)| BucketReport {
                bucket,
                positions: bucket_positions[bucket],
                rates: StageRates::from_counts(c),
            })
            .collect(),
    };

    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    serde_json::to_writer_pretty(&mut locked, &report)?;
    use std::io::Write;
    writeln!(locked)?;

    if let Some(path) = &cli.out {
        let mut writer = std::io::BufWriter::new(
            std::fs::File::create(path)
                .with_context(|| format!("出力できません: {}", path.display()))?,
        );
        serde_json::to_writer_pretty(&mut writer, &report)?;
        writeln!(writer)?;
    }
    Ok(())
}

/// CLI entrypoint。
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    let weights = load_progress_coeff_kpabs(&cli.progress_coeff)
        .map_err(|e| anyhow::anyhow!("--progress-coeff を読めません: {e}"))?;
    set_layer_stack_progress_kpabs_weights(weights)
        .map_err(|e| anyhow::anyhow!("progress 設定に失敗しました: {e}"))?;
    set_layer_stack_bucket_mode(LayerStackBucketMode::Progress8KPAbs);

    let network = NNUENetwork::load(&cli.nnue)
        .with_context(|| format!("NNUE を読み込めません: {:?}", cli.nnue))?;
    let ls_net = match &network {
        NNUENetwork::LayerStacks(net) => net,
        _ => anyhow::bail!("nnue_saturation は LayerStacks NNUE のみ対応"),
    };

    ls_dispatch_ft_size!(
        ls_net,
        |concrete_net| run_for_network(&cli, concrete_net),
        _ => anyhow::bail!("有効な LayerStacks (FT × L1) バリアントがありません"),
    )
}
