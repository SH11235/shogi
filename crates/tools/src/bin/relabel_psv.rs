use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use rshogi_core::position::Position;
use rshogi_core::types::{EnteringKingRule, Move};
use serde::Serialize;
use serde_json::Value;
use tools::common::dedup::canonicalize_maybe_new;
use tools::packed_sfen::{PackedSfenValue, unpack_sfen};

const GAP_HISTOGRAM_BOUNDARIES_CP: [i32; 8] = [0, 50, 100, 200, 300, 500, 1000, 3000];

#[derive(Debug, Parser)]
#[command(about = "PSV の score を手番側視点の game_result 由来値へ置換する")]
struct Cli {
    /// 入力 PSV。カンマ区切りと glob を使用でき、展開後は辞書順で処理する。
    #[arg(long, required = true, value_delimiter = ',')]
    input: Vec<String>,

    /// 出力 PSV。--dry-run では省略でき、指定しても作成しない。
    #[arg(long)]
    output: Option<PathBuf>,

    /// 勝敗を置換する絶対 score（勝ち=正、負け=負、引分=0）
    #[arg(long, default_value_t = 2500)]
    win_cp: i16,

    /// 手番側が27点法で宣言勝ち可能なら score を +win-cp にする
    #[arg(long, default_value_t = false)]
    declaration_override: bool,

    /// diversions より前の局面を除外する
    #[arg(long, default_value_t = false)]
    deblunder: bool,

    /// 入力 PSV と 1:1・同順の u32 little-endian game_id sidecar
    #[arg(long)]
    game_id_sidecar: Option<PathBuf>,

    /// gensfen result JSONL。カンマ区切りと glob を使用できる。
    #[arg(long, value_delimiter = ',')]
    diversions: Vec<String>,

    /// diversion が複数ある対局で使う除外境界
    #[arg(long, value_enum)]
    deblunder_mode: Option<DeblunderMode>,

    /// 元 score による勝敗符号判定を使う最小絶対値
    #[arg(long, default_value_t = 300)]
    flip_threshold: i32,

    /// score_gap_cp だけで汚染とする最小値
    #[arg(long, default_value_t = 100)]
    gap_threshold: i32,

    /// PSV を書かず、判定と統計だけ実行する
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// 入力 PSV と 1:1・同順の u8 verdict sidecar
    #[arg(long)]
    emit_verdict_sidecar: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DeblunderMode {
    /// 最後の diversion ply 以前を除外する
    #[value(name = "drop-before-last")]
    Last,
    /// 最初の diversion ply 以前を除外する
    #[value(name = "drop-before-any")]
    Any,
    /// 元 score、game_result、score_gap_cp の整合性で汚染 diversion を選ぶ
    #[value(name = "drop-contaminated")]
    Contaminated,
}

#[derive(Clone, Copy, Debug)]
struct DiversionBounds {
    first: u16,
    last: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiversionKind {
    MultiPv,
    Random,
}

#[derive(Clone, Copy, Debug)]
struct Diversion {
    ply: u16,
    kind: DiversionKind,
    gap_cp: Option<i32>,
}

#[derive(Clone, Copy, Debug)]
enum GameEndReason {
    MaxMoves,
    Sennichite,
    Other,
}

#[derive(Debug)]
struct GameInfo {
    reason: GameEndReason,
    diversions: Vec<Diversion>,
    finished: Cell<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Verdict {
    Kept = 0,
    DroppedFlip = 1,
    DroppedGap = 2,
    DroppedMissingRecord = 3,
    DroppedRandom = 4,
    DroppedLegacy = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContaminationReason {
    Flip,
    Gap,
    MissingRecord,
    Random,
}

impl ContaminationReason {
    fn verdict(self) -> Verdict {
        match self {
            Self::Flip => Verdict::DroppedFlip,
            Self::Gap => Verdict::DroppedGap,
            Self::MissingRecord => Verdict::DroppedMissingRecord,
            Self::Random => Verdict::DroppedRandom,
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Gap => 0,
            Self::Flip => 1,
            Self::MissingRecord => 2,
            Self::Random => 3,
        }
    }
}

#[derive(Default, Serialize)]
struct DecisionBreakdown {
    flip_contaminated: u64,
    gap_contaminated: u64,
    missing_record_contaminated: u64,
    missing_record_preserved: u64,
    random_contaminated: u64,
}

#[derive(Default, Serialize)]
struct DrawReasonStats {
    max_moves: u64,
    sennichite: u64,
    other: u64,
}

#[derive(Serialize)]
struct GapHistogram {
    boundaries_cp: [i32; 8],
    counts: [u64; 9],
}

impl Default for GapHistogram {
    fn default() -> Self {
        Self {
            boundaries_cp: GAP_HISTOGRAM_BOUNDARIES_CP,
            counts: [0; 9],
        }
    }
}

impl GapHistogram {
    fn observe(&mut self, gap_cp: i32) {
        let bucket = self
            .boundaries_cp
            .iter()
            .position(|boundary| gap_cp < *boundary)
            .unwrap_or(self.counts.len() - 1);
        self.counts[bucket] += 1;
    }
}

#[derive(Default, Serialize)]
struct Stats {
    input_positions: u64,
    wins: u64,
    losses: u64,
    draws: u64,
    declaration_overrides: u64,
    declaration_overrides_dropped: u64,
    deblunder_dropped_positions: u64,
    diversion_games: u64,
    contaminated_games: u64,
    preserved_games: u64,
    gap_histogram: GapHistogram,
    decisions: DecisionBreakdown,
    draw_games_by_reason: DrawReasonStats,
}

struct BufferedRecord {
    record: PackedSfenValue,
    original_score: i16,
    input_path_index: usize,
    input_record_index: u64,
}

#[derive(Clone, Copy)]
struct DropBoundary {
    ply: u16,
    reason: ContaminationReason,
}

struct RecordSinks<'a> {
    output: &'a mut Option<BufWriter<File>>,
    verdict: &'a mut Option<BufWriter<File>>,
    declaration_pos: &'a mut Option<Position>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    validate_cli(&cli)?;
    let input_paths = expand_paths(&cli.input, "--input")?;
    let diversion_paths = if cli.deblunder {
        expand_paths(&cli.diversions, "--diversions")?
    } else {
        Vec::new()
    };
    validate_generated_paths(&cli, &input_paths, &diversion_paths)?;

    let mut output = if cli.dry_run {
        None
    } else {
        let path = cli.output.as_deref().context("internal error: validated --output is absent")?;
        Some(create_writer(path, "output")?)
    };
    let mut verdict_writer = cli
        .emit_verdict_sidecar
        .as_deref()
        .map(|path| create_writer(path, "verdict sidecar"))
        .transpose()?;
    let mut sidecar = cli
        .game_id_sidecar
        .as_deref()
        .map(|path| {
            File::open(path)
                .map(BufReader::new)
                .with_context(|| format!("failed to open game_id sidecar {}", path.display()))
        })
        .transpose()?;
    let mut declaration_pos = cli.declaration_override.then(Position::new);
    let mut stats = Stats::default();
    let mut sinks = RecordSinks {
        output: &mut output,
        verdict: &mut verdict_writer,
        declaration_pos: &mut declaration_pos,
    };

    let deblunder_mode = cli.deblunder_mode.unwrap_or(DeblunderMode::Last);
    match deblunder_mode {
        DeblunderMode::Contaminated => {
            let games = load_game_info(&diversion_paths)?;
            run_drop_contaminated(
                &cli,
                &input_paths,
                &games,
                &mut sidecar,
                &mut sinks,
                &mut stats,
            )?;
        }
        DeblunderMode::Last | DeblunderMode::Any => {
            let bounds = if cli.deblunder {
                load_diversion_bounds(&diversion_paths)?
            } else {
                HashMap::new()
            };
            run_streaming(
                &cli,
                deblunder_mode == DeblunderMode::Last,
                &input_paths,
                &bounds,
                &mut sidecar,
                &mut sinks,
                &mut stats,
            )?;
        }
    }

    validate_sidecar_end(&cli, &mut sidecar)?;
    if let Some(writer) = sinks.output.as_mut() {
        writer.flush()?;
    }
    if let Some(writer) = sinks.verdict.as_mut() {
        writer.flush()?;
    }
    eprintln!("{}", serde_json::to_string(&stats)?);
    Ok(())
}

fn validate_cli(cli: &Cli) -> Result<()> {
    if !(1..32000).contains(&cli.win_cp) {
        bail!("--win-cp must be in 1..32000");
    }
    if !(0..=10000).contains(&cli.flip_threshold) {
        bail!("--flip-threshold must be in 0..=10000");
    }
    if !(0..=10000).contains(&cli.gap_threshold) {
        bail!("--gap-threshold must be in 0..=10000");
    }
    if !cli.dry_run && cli.output.is_none() {
        bail!("--output is required unless --dry-run is used");
    }
    if cli.deblunder && cli.game_id_sidecar.is_none() {
        bail!("--deblunder requires --game-id-sidecar");
    }
    if cli.deblunder && cli.diversions.is_empty() {
        bail!("--deblunder requires --diversions");
    }
    if !cli.deblunder && (cli.game_id_sidecar.is_some() || !cli.diversions.is_empty()) {
        bail!("--game-id-sidecar and --diversions require --deblunder");
    }
    if !cli.deblunder && cli.deblunder_mode.is_some() {
        bail!("--deblunder-mode requires --deblunder; add --deblunder or remove --deblunder-mode");
    }
    Ok(())
}

fn create_writer(path: &Path, label: &str) -> Result<BufWriter<File>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    File::create(path)
        .map(BufWriter::new)
        .with_context(|| format!("failed to create {label} {}", path.display()))
}

fn run_streaming(
    cli: &Cli,
    use_last_diversion: bool,
    input_paths: &[PathBuf],
    bounds: &HashMap<u32, DiversionBounds>,
    sidecar: &mut Option<BufReader<File>>,
    sinks: &mut RecordSinks<'_>,
    stats: &mut Stats,
) -> Result<()> {
    for input_path in input_paths {
        let mut input = open_input(input_path)?;
        while let Some(bytes) =
            read_fixed::<{ PackedSfenValue::SIZE }>(&mut input, input_path, "PSV record")?
        {
            let mut record = PackedSfenValue::from_bytes(&bytes)
                .context("failed to decode fixed-size PSV record")?;
            stats.input_positions += 1;
            let game_id = read_game_id(cli, sidecar, stats.input_positions)?;
            let drop = game_id.and_then(|id| bounds.get(&id)).is_some_and(|game_bounds| {
                let boundary = if use_last_diversion {
                    game_bounds.last
                } else {
                    game_bounds.first
                };
                record.game_ply <= boundary
            });
            let overridden = relabel_record(
                cli,
                &mut record,
                input_path,
                stats.input_positions,
                sinks.declaration_pos,
                stats,
            )?;
            flush_record(
                cli,
                record,
                drop.then_some(Verdict::DroppedLegacy),
                overridden,
                sinks.output,
                sinks.verdict,
                stats,
            )?;
        }
    }
    Ok(())
}

fn run_drop_contaminated(
    cli: &Cli,
    input_paths: &[PathBuf],
    games: &HashMap<u32, GameInfo>,
    sidecar: &mut Option<BufReader<File>>,
    sinks: &mut RecordSinks<'_>,
    stats: &mut Stats,
) -> Result<()> {
    let mut current_game_id = None;
    let mut records = Vec::new();
    for (input_path_index, input_path) in input_paths.iter().enumerate() {
        let mut input = open_input(input_path)?;
        let mut input_record_index = 0;
        while let Some(bytes) =
            read_fixed::<{ PackedSfenValue::SIZE }>(&mut input, input_path, "PSV record")?
        {
            let record = PackedSfenValue::from_bytes(&bytes)
                .context("failed to decode fixed-size PSV record")?;
            stats.input_positions += 1;
            input_record_index += 1;
            let game_id = read_game_id(cli, sidecar, stats.input_positions)?
                .context("internal error: drop-contaminated sidecar is absent")?;
            let game_info = games.get(&game_id).with_context(|| {
                format!(
                    "game_id {game_id} at record {input_record_index} in {} is absent from result JSONL",
                    input_path.display()
                )
            })?;

            if current_game_id.is_some_and(|current| current != game_id) {
                let finished = current_game_id.context("internal error: current game is absent")?;
                process_game(cli, finished, &mut records, input_paths, games, sinks, stats)?;
                games
                    .get(&finished)
                    .context("internal error: current game metadata is absent")?
                    .finished
                    .set(true);
                if game_info.finished.get() {
                    bail!(
                        "game_id {game_id} reappeared non-contiguously at record {input_record_index} in {}",
                        input_path.display()
                    );
                }
            }
            current_game_id = Some(game_id);
            records.push(BufferedRecord {
                original_score: record.score,
                record,
                input_path_index,
                input_record_index,
            });
        }
    }

    if let Some(game_id) = current_game_id {
        process_game(cli, game_id, &mut records, input_paths, games, sinks, stats)?;
    }
    Ok(())
}

fn process_game(
    cli: &Cli,
    game_id: u32,
    records: &mut Vec<BufferedRecord>,
    input_paths: &[PathBuf],
    games: &HashMap<u32, GameInfo>,
    sinks: &mut RecordSinks<'_>,
    stats: &mut Stats,
) -> Result<()> {
    let info = games.get(&game_id).context("internal error: game metadata is absent")?;
    if records.first().is_some_and(|record| record.record.game_result == 0) {
        match info.reason {
            GameEndReason::MaxMoves => stats.draw_games_by_reason.max_moves += 1,
            GameEndReason::Sennichite => stats.draw_games_by_reason.sennichite += 1,
            GameEndReason::Other => stats.draw_games_by_reason.other += 1,
        }
    }
    let boundary = assess_game(cli, records, info, stats)?;

    for buffered in records.drain(..) {
        let mut record = buffered.record;
        let drop_reason = boundary
            .filter(|drop_boundary| record.game_ply <= drop_boundary.ply)
            .map(|drop_boundary| drop_boundary.reason.verdict());
        let overridden = relabel_record(
            cli,
            &mut record,
            &input_paths[buffered.input_path_index],
            buffered.input_record_index,
            sinks.declaration_pos,
            stats,
        )?;
        flush_record(cli, record, drop_reason, overridden, sinks.output, sinks.verdict, stats)?;
    }
    Ok(())
}

fn assess_game(
    cli: &Cli,
    records: &[BufferedRecord],
    info: &GameInfo,
    stats: &mut Stats,
) -> Result<Option<DropBoundary>> {
    if info.diversions.is_empty() {
        return Ok(None);
    }
    stats.diversion_games += 1;
    let mut boundary: Option<DropBoundary> = None;

    for diversion in &info.diversions {
        if let Some(gap) = diversion.gap_cp {
            stats.gap_histogram.observe(gap);
        }
        let contamination = match diversion.kind {
            DiversionKind::Random => {
                stats.decisions.random_contaminated += 1;
                Some(ContaminationReason::Random)
            }
            DiversionKind::MultiPv => {
                let gap = diversion
                    .gap_cp
                    .context("multipv diversion requires score_gap_cp in drop-contaminated mode")?;
                match records.iter().find(|record| record.record.game_ply == diversion.ply) {
                    None if gap >= cli.gap_threshold => {
                        stats.decisions.missing_record_contaminated += 1;
                        Some(ContaminationReason::MissingRecord)
                    }
                    None => {
                        stats.decisions.missing_record_preserved += 1;
                        None
                    }
                    Some(record) => {
                        let result = record.record.game_result;
                        let score = i32::from(record.original_score);
                        if result != 0 && score.abs() >= cli.flip_threshold {
                            if score.signum() == i32::from(result).signum() {
                                None
                            } else {
                                stats.decisions.flip_contaminated += 1;
                                Some(ContaminationReason::Flip)
                            }
                        } else if gap >= cli.gap_threshold {
                            stats.decisions.gap_contaminated += 1;
                            Some(ContaminationReason::Gap)
                        } else {
                            None
                        }
                    }
                }
            }
        };

        if let Some(reason) = contamination {
            let candidate = DropBoundary {
                ply: diversion.ply,
                reason,
            };
            if boundary.is_none_or(|current| {
                candidate.ply > current.ply
                    || (candidate.ply == current.ply
                        && candidate.reason.priority() > current.reason.priority())
            }) {
                boundary = Some(candidate);
            }
        }
    }

    if boundary.is_some() {
        stats.contaminated_games += 1;
    } else {
        stats.preserved_games += 1;
    }
    Ok(boundary)
}

fn relabel_record(
    cli: &Cli,
    record: &mut PackedSfenValue,
    input_path: &Path,
    record_index: u64,
    declaration_pos: &mut Option<Position>,
    stats: &mut Stats,
) -> Result<bool> {
    match record.game_result {
        1 => {
            stats.wins += 1;
            record.score = cli.win_cp;
        }
        -1 => {
            stats.losses += 1;
            record.score = -cli.win_cp;
        }
        0 => {
            stats.draws += 1;
            record.score = 0;
        }
        value => bail!(
            "invalid game_result {value} at record {record_index} in {}",
            input_path.display()
        ),
    }

    let mut overridden = false;
    if let Some(pos) = declaration_pos.as_mut() {
        let sfen = unpack_sfen(&record.sfen).map_err(|error| {
            anyhow::anyhow!(
                "failed to unpack record {record_index} in {}: {error}",
                input_path.display()
            )
        })?;
        pos.set_sfen(&sfen).map_err(|error| {
            anyhow::anyhow!(
                "failed to decode record {record_index} in {}: {error}",
                input_path.display()
            )
        })?;
        if pos.declaration_win(EnteringKingRule::Point27) == Move::WIN {
            record.score = cli.win_cp;
            stats.declaration_overrides += 1;
            overridden = true;
        }
    }
    Ok(overridden)
}

fn flush_record(
    cli: &Cli,
    record: PackedSfenValue,
    drop_verdict: Option<Verdict>,
    declaration_overridden: bool,
    output: &mut Option<BufWriter<File>>,
    verdict_writer: &mut Option<BufWriter<File>>,
    stats: &mut Stats,
) -> Result<()> {
    let verdict = drop_verdict.unwrap_or(Verdict::Kept);
    if let Some(writer) = verdict_writer.as_mut() {
        let path = cli
            .emit_verdict_sidecar
            .as_deref()
            .context("internal error: verdict writer path is absent")?;
        writer
            .write_all(&[verdict as u8])
            .with_context(|| format!("failed to write verdict sidecar {}", path.display()))?;
    }
    if drop_verdict.is_some() {
        stats.deblunder_dropped_positions += 1;
        if declaration_overridden {
            stats.declaration_overrides_dropped += 1;
        }
    } else if let Some(writer) = output.as_mut() {
        let path = cli.output.as_deref().context("internal error: output writer path is absent")?;
        writer
            .write_all(&record.to_bytes())
            .with_context(|| format!("failed to write output {}", path.display()))?;
    }
    Ok(())
}

fn open_input(path: &Path) -> Result<BufReader<File>> {
    File::open(path)
        .map(BufReader::new)
        .with_context(|| format!("failed to open input {}", path.display()))
}

fn read_game_id(
    cli: &Cli,
    sidecar: &mut Option<BufReader<File>>,
    record_index: u64,
) -> Result<Option<u32>> {
    let Some(reader) = sidecar.as_mut() else {
        return Ok(None);
    };
    let path = cli
        .game_id_sidecar
        .as_deref()
        .context("internal error: sidecar reader path is absent")?;
    let id_bytes = read_fixed::<4>(reader, path, "game_id")?
        .ok_or_else(|| anyhow::anyhow!("game_id sidecar ended before PSV record {record_index}"))?;
    Ok(Some(u32::from_le_bytes(id_bytes)))
}

fn validate_sidecar_end(cli: &Cli, sidecar: &mut Option<BufReader<File>>) -> Result<()> {
    if let Some(reader) = sidecar.as_mut() {
        let path = cli
            .game_id_sidecar
            .as_deref()
            .context("internal error: sidecar reader path is absent")?;
        if read_fixed::<4>(reader, path, "game_id")?.is_some() {
            bail!("game_id sidecar has more entries than the input PSV files");
        }
    }
    Ok(())
}

fn expand_paths(patterns: &[String], option: &str) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for pattern in patterns {
        let mut matched = false;
        for entry in
            glob::glob(pattern).with_context(|| format!("invalid glob for {option}: {pattern}"))?
        {
            let path = entry.with_context(|| format!("failed to expand {option}: {pattern}"))?;
            if path.is_file() {
                paths.insert(path);
                matched = true;
            }
        }
        if !matched {
            bail!("{option} matched no files: {pattern}");
        }
    }
    if paths.is_empty() {
        bail!("{option} matched no files");
    }
    Ok(paths.into_iter().collect())
}

fn validate_generated_paths(
    cli: &Cli,
    input_paths: &[PathBuf],
    diversion_paths: &[PathBuf],
) -> Result<()> {
    let mut protected: Vec<(&Path, &str)> =
        input_paths.iter().map(|path| (path.as_path(), "--input")).collect();
    if let Some(path) = cli.game_id_sidecar.as_deref() {
        protected.push((path, "--game-id-sidecar"));
    }
    protected.extend(diversion_paths.iter().map(|path| (path.as_path(), "--diversions")));
    let output = (!cli.dry_run).then_some(cli.output.as_deref()).flatten();
    if let Some(path) = output {
        validate_generated_path(path, "--output", &protected)?;
    }
    if let Some(path) = cli.emit_verdict_sidecar.as_deref() {
        validate_generated_path(path, "--emit-verdict-sidecar", &protected)?;
        if let Some(output_path) = output {
            let normalized = canonicalize_maybe_new(path).with_context(|| {
                format!("failed to normalize --emit-verdict-sidecar {}", path.display())
            })?;
            let normalized_output = canonicalize_maybe_new(output_path).with_context(|| {
                format!("failed to normalize --output {}", output_path.display())
            })?;
            if normalized == normalized_output || same_inode(path, output_path)? {
                bail!(
                    "generated path {} resolves to the same file as --output {}; refusing to truncate it",
                    path.display(),
                    output_path.display()
                );
            }
        }
    }
    Ok(())
}

fn validate_generated_path(path: &Path, label: &str, protected: &[(&Path, &str)]) -> Result<()> {
    let normalized = canonicalize_maybe_new(path)
        .with_context(|| format!("failed to normalize {label} {}", path.display()))?;
    for (protected_path, protected_label) in protected {
        validate_distinct_file(path, &normalized, protected_path, protected_label)?;
    }
    Ok(())
}

fn validate_distinct_file(
    output: &Path,
    normalized_output: &Path,
    protected: &Path,
    option: &str,
) -> Result<()> {
    let canonical_protected = protected
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {option} {}", protected.display()))?;
    if normalized_output == canonical_protected || same_inode(output, protected)? {
        bail!(
            "generated path {} resolves to the same file as {option} {}; refusing to truncate it",
            output.display(),
            protected.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn same_inode(a: &Path, b: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let a_metadata = match a.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to stat output {}", a.display()));
        }
    };
    let b_metadata = b
        .metadata()
        .with_context(|| format!("failed to stat protected input {}", b.display()))?;
    Ok(a_metadata.dev() == b_metadata.dev() && a_metadata.ino() == b_metadata.ino())
}

#[cfg(not(unix))]
fn same_inode(_a: &Path, _b: &Path) -> Result<bool> {
    Ok(false)
}

fn load_diversion_bounds(paths: &[PathBuf]) -> Result<HashMap<u32, DiversionBounds>> {
    let mut bounds = HashMap::new();
    for path in paths {
        let reader = BufReader::new(
            File::open(path)
                .with_context(|| format!("failed to open diversions {}", path.display()))?,
        );
        for (line_index, line) in reader.lines().enumerate() {
            let line = line.with_context(|| {
                format!("failed to read {} line {}", path.display(), line_index + 1)
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line).with_context(|| {
                format!("invalid JSON in {} line {}", path.display(), line_index + 1)
            })?;
            if value.get("type").and_then(Value::as_str) != Some("result") {
                continue;
            }
            let game_id = parse_game_id(&value, path, line_index)?;
            let start_ply = parse_start_ply(&value, path, line_index)?;
            let diversions =
                value.get("diversions").and_then(Value::as_array).with_context(|| {
                    format!(
                        "missing diversions in {} line {} (gensfen --omit-diversions で生成した \
                     jsonl には diversions が無い。deblunder には全量記録の run が必要)",
                        path.display(),
                        line_index + 1
                    )
                })?;
            for diversion in diversions {
                let relative_ply = parse_relative_ply(diversion, path, line_index)?;
                let ply = absolute_ply(start_ply, relative_ply, path, line_index)?;
                bounds
                    .entry(game_id)
                    .and_modify(|entry: &mut DiversionBounds| {
                        entry.first = entry.first.min(ply);
                        entry.last = entry.last.max(ply);
                    })
                    .or_insert(DiversionBounds {
                        first: ply,
                        last: ply,
                    });
            }
        }
    }
    Ok(bounds)
}

fn load_game_info(paths: &[PathBuf]) -> Result<HashMap<u32, GameInfo>> {
    let mut games = HashMap::new();
    for path in paths {
        let reader = BufReader::new(
            File::open(path)
                .with_context(|| format!("failed to open diversions {}", path.display()))?,
        );
        for (line_index, line) in reader.lines().enumerate() {
            let line = line.with_context(|| {
                format!("failed to read {} line {}", path.display(), line_index + 1)
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line).with_context(|| {
                format!("invalid JSON in {} line {}", path.display(), line_index + 1)
            })?;
            if value.get("type").and_then(Value::as_str) != Some("result") {
                continue;
            }
            let game_id = parse_game_id(&value, path, line_index)?;
            let start_ply = parse_start_ply(&value, path, line_index)?;
            let reason = match value.get("reason").and_then(Value::as_str) {
                Some("max_moves") => GameEndReason::MaxMoves,
                Some("sennichite") => GameEndReason::Sennichite,
                _ => GameEndReason::Other,
            };
            let values = value.get("diversions").and_then(Value::as_array).with_context(|| {
                format!(
                    "missing diversions in {} line {} (gensfen --omit-diversions で生成した \
                     jsonl には diversions が無い。deblunder には全量記録の run が必要)",
                    path.display(),
                    line_index + 1
                )
            })?;
            let mut diversions = Vec::with_capacity(values.len());
            for diversion in values {
                diversions.push(parse_diversion(diversion, start_ply, path, line_index)?);
            }
            if games
                .insert(
                    game_id,
                    GameInfo {
                        reason,
                        diversions,
                        finished: Cell::new(false),
                    },
                )
                .is_some()
            {
                bail!(
                    "duplicate result for game_id {game_id} in {} line {}",
                    path.display(),
                    line_index + 1
                );
            }
        }
    }
    Ok(games)
}

fn parse_game_id(value: &Value, path: &Path, line_index: usize) -> Result<u32> {
    value
        .get("game_id")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .with_context(|| format!("invalid game_id in {} line {}", path.display(), line_index + 1))
}

fn parse_start_ply(value: &Value, path: &Path, line_index: usize) -> Result<u32> {
    value
        .get("start_sfen")
        .and_then(Value::as_str)
        .and_then(|sfen| sfen.split_whitespace().nth(3))
        .and_then(|ply| ply.parse::<u32>().ok())
        .with_context(|| {
            format!("invalid start_sfen ply in {} line {}", path.display(), line_index + 1)
        })
}

fn parse_diversion(
    value: &Value,
    start_ply: u32,
    path: &Path,
    line_index: usize,
) -> Result<Diversion> {
    let relative_ply = parse_relative_ply(value, path, line_index)?;
    let ply = absolute_ply(start_ply, relative_ply, path, line_index)?;
    let kind = match value.get("kind").and_then(Value::as_str) {
        Some("random") => DiversionKind::Random,
        Some("multipv") | None => DiversionKind::MultiPv,
        Some(kind) => {
            bail!("invalid diversion kind {kind:?} in {} line {}", path.display(), line_index + 1)
        }
    };
    let gap_cp = value
        .get("score_gap_cp")
        .and_then(Value::as_i64)
        .and_then(|gap| i32::try_from(gap).ok())
        .map(|gap| gap.clamp(-10000, 10000));
    Ok(Diversion { ply, kind, gap_cp })
}

fn parse_relative_ply(value: &Value, path: &Path, line_index: usize) -> Result<u32> {
    value
        .get("ply")
        .and_then(Value::as_u64)
        .and_then(|ply| u32::try_from(ply).ok())
        .with_context(|| {
            format!("invalid diversion ply in {} line {}", path.display(), line_index + 1)
        })
}

fn absolute_ply(start_ply: u32, relative_ply: u32, path: &Path, line_index: usize) -> Result<u16> {
    relative_ply
        .checked_sub(1)
        .and_then(|offset| start_ply.checked_add(offset))
        .and_then(|ply| u16::try_from(ply).ok())
        .with_context(|| {
            format!(
                "diversion ply overflow in {} line {}: start_ply={start_ply}, relative_ply={relative_ply}",
                path.display(),
                line_index + 1
            )
        })
}

fn read_fixed<const N: usize>(
    reader: &mut impl Read,
    path: &Path,
    record_name: &str,
) -> Result<Option<[u8; N]>> {
    let mut bytes = [0u8; N];
    let read = reader
        .read(&mut bytes[..1])
        .with_context(|| format!("failed to read {record_name} from {}", path.display()))?;
    if read == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut bytes[1..])
        .with_context(|| format!("truncated {record_name} at end of {}", path.display()))?;
    Ok(Some(bytes))
}
