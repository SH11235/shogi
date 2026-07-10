use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use rshogi_core::position::Position;
use rshogi_core::types::{EnteringKingRule, Move};
use serde_json::Value;
use tools::packed_sfen::{PackedSfenValue, unpack_sfen};

#[derive(Debug, Parser)]
#[command(about = "PSV の score を手番側視点の game_result 由来値へ置換する")]
struct Cli {
    /// 入力 PSV。カンマ区切りと glob を使用でき、展開後は辞書順で処理する。
    #[arg(long, required = true, value_delimiter = ',')]
    input: Vec<String>,

    /// 出力 PSV
    #[arg(long)]
    output: PathBuf,

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
    #[arg(long, value_enum, default_value_t = DeblunderMode::DropBeforeLast)]
    deblunder_mode: DeblunderMode,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DeblunderMode {
    /// 最後の diversion ply 以前を除外する
    DropBeforeLast,
    /// 最初の diversion ply 以前を除外する
    DropBeforeAny,
}

#[derive(Clone, Copy, Debug)]
struct DiversionBounds {
    first: u16,
    last: u16,
}

#[derive(Default)]
struct Stats {
    input: u64,
    wins: u64,
    losses: u64,
    draws: u64,
    declaration_overrides: u64,
    deblunder_drops: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    if !(1..32000).contains(&cli.win_cp) {
        bail!("--win-cp must be in 1..32000");
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

    let input_paths = expand_paths(&cli.input, "--input")?;
    if input_paths.iter().any(|path| path == &cli.output) {
        bail!("--output must differ from every input path");
    }
    if cli.game_id_sidecar.as_deref() == Some(cli.output.as_path()) {
        bail!("--output must differ from --game-id-sidecar");
    }
    let diversion_bounds = if cli.deblunder {
        load_diversion_bounds(&expand_paths(&cli.diversions, "--diversions")?)?
    } else {
        HashMap::new()
    };

    if let Some(parent) = cli.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut output = BufWriter::new(
        File::create(&cli.output)
            .with_context(|| format!("failed to create {}", cli.output.display()))?,
    );
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

    for input_path in &input_paths {
        let mut input = BufReader::new(
            File::open(input_path)
                .with_context(|| format!("failed to open input {}", input_path.display()))?,
        );
        while let Some(bytes) =
            read_fixed::<{ PackedSfenValue::SIZE }>(&mut input, input_path, "PSV record")?
        {
            stats.input += 1;
            let mut record = PackedSfenValue::from_bytes(&bytes)
                .expect("fixed-size PSV record must always decode");
            let game_id = if let Some(reader) = sidecar.as_mut() {
                let path = cli.game_id_sidecar.as_deref().expect("validated sidecar path");
                let id_bytes = read_fixed::<4>(reader, path, "game_id")?.ok_or_else(|| {
                    anyhow::anyhow!("game_id sidecar ended before PSV record {}", stats.input)
                })?;
                Some(u32::from_le_bytes(id_bytes))
            } else {
                None
            };

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
                    "invalid game_result {value} at record {} in {}",
                    stats.input,
                    input_path.display()
                ),
            }

            if let Some(pos) = declaration_pos.as_mut() {
                let sfen = unpack_sfen(&record.sfen).map_err(|error| {
                    anyhow::anyhow!(
                        "failed to unpack record {} in {}: {error}",
                        stats.input,
                        input_path.display()
                    )
                })?;
                pos.set_sfen(&sfen).map_err(|error| {
                    anyhow::anyhow!(
                        "failed to decode record {} in {}: {error}",
                        stats.input,
                        input_path.display()
                    )
                })?;
                if pos.declaration_win(EnteringKingRule::Point27) == Move::WIN {
                    record.score = cli.win_cp;
                    stats.declaration_overrides += 1;
                }
            }

            let drop = game_id.and_then(|id| diversion_bounds.get(&id)).is_some_and(|bounds| {
                let boundary = match cli.deblunder_mode {
                    DeblunderMode::DropBeforeLast => bounds.last,
                    DeblunderMode::DropBeforeAny => bounds.first,
                };
                record.game_ply <= boundary
            });
            if drop {
                stats.deblunder_drops += 1;
            } else {
                output
                    .write_all(&record.to_bytes())
                    .with_context(|| format!("failed to write output {}", cli.output.display()))?;
            }
        }
    }

    if let Some(reader) = sidecar.as_mut() {
        let path = cli.game_id_sidecar.as_deref().expect("validated sidecar path");
        if read_fixed::<4>(reader, path, "game_id")?.is_some() {
            bail!("game_id sidecar has more entries than the input PSV files");
        }
    }
    output.flush()?;
    eprintln!(
        "input={} win={} loss={} draw={} declaration_override={} deblunder_drop={}",
        stats.input,
        stats.wins,
        stats.losses,
        stats.draws,
        stats.declaration_overrides,
        stats.deblunder_drops
    );
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
            let game_id = value
                .get("game_id")
                .and_then(Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .with_context(|| {
                    format!("invalid game_id in {} line {}", path.display(), line_index + 1)
                })?;
            let start_sfen =
                value.get("start_sfen").and_then(Value::as_str).with_context(|| {
                    format!("missing start_sfen in {} line {}", path.display(), line_index + 1)
                })?;
            let start_ply = start_sfen
                .split_whitespace()
                .nth(3)
                .and_then(|ply| ply.parse::<u32>().ok())
                .with_context(|| {
                    format!("invalid start_sfen ply in {} line {}", path.display(), line_index + 1)
                })?;
            let diversions =
                value.get("diversions").and_then(Value::as_array).with_context(|| {
                    format!("missing diversions in {} line {}", path.display(), line_index + 1)
                })?;
            for diversion in diversions {
                let relative_ply = diversion
                    .get("ply")
                    .and_then(Value::as_u64)
                    .and_then(|ply| u32::try_from(ply).ok())
                    .with_context(|| {
                        format!(
                            "invalid diversion ply in {} line {}",
                            path.display(),
                            line_index + 1
                        )
                    })?;
                let ply = relative_ply
                    .checked_sub(1)
                    .and_then(|offset| start_ply.checked_add(offset))
                    .and_then(|ply| u16::try_from(ply).ok())
                    .with_context(|| {
                        format!(
                            "diversion ply overflow in {} line {}: start_ply={}, relative_ply={}",
                            path.display(),
                            line_index + 1,
                            start_ply,
                            relative_ply
                        )
                    })?;
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

fn read_fixed<const N: usize>(
    reader: &mut impl Read,
    path: &Path,
    record_name: &str,
) -> Result<Option<[u8; N]>> {
    let mut bytes = [0u8; N];
    let read = reader
        .read(&mut bytes[..1])
        .with_context(|| format!("failed to read {} from {}", record_name, path.display()))?;
    if read == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut bytes[1..])
        .with_context(|| format!("truncated {} at end of {}", record_name, path.display()))?;
    Ok(Some(bytes))
}
