//! floodgate CSA 由来の入玉アンカー局面を gensfen 用 startpos に変換する。
//!
//! 候補は安定ハッシュでディスクへ分割し、パーティション単位で exact dedup する。
//! 実行中の状態は `<out-dir>.work/state.json` に保存し、`--resume` で再開できる。
//! 完了時だけ work ディレクトリを `out-dir` へ rename して成果物を公開する。

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::Parser;
use rshogi_core::position::Position as CorePosition;
use rshogi_core::types::{EnteringKingRule, Move};
use rshogi_csa::{ParsedMove, parse_csa_full};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_PARTITIONS: usize = 128;
const DEFAULT_CHECKPOINT_INTERVAL: usize = 1_000_000;
const MAX_OPEN_PARTITION_WRITERS: usize = 32;
const STATE_VERSION: u32 = 1;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "CSA manifest から入玉アンカー開始局面を抽出する"
)]
struct Cli {
    /// TSV: csa_path, black_entry_ply, white_entry_ply, total_plies
    #[arg(long)]
    manifest: PathBuf,

    /// startpos.txt と provenance.tsv の出力先（既存ディレクトリは上書きしない）
    #[arg(long)]
    out_dir: PathBuf,

    /// exact dedup 用のディスクパーティション数
    #[arg(long, default_value_t = DEFAULT_PARTITIONS, value_parser = parse_partition_count)]
    partitions: usize,

    /// `<out-dir>.work` の checkpoint から再開する
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// manifest の処理行数ごとに partition を flush して checkpoint を保存する
    #[arg(long, default_value_t = DEFAULT_CHECKPOINT_INTERVAL, value_parser = parse_positive_usize)]
    checkpoint_interval: usize,
}

#[derive(Debug, Clone)]
struct ManifestRow {
    csa_path: PathBuf,
    black_entry_ply: i32,
    white_entry_ply: i32,
    total_plies: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnchorCandidate {
    anchor_ply: u32,
    anchor_kind: &'static str,
    entry_side: char,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExtractedPosition {
    /// 手数を除いた exact dedup キー（盤面・手番・持ち駒）。
    position_key: String,
    sfen: String,
    source_csa: PathBuf,
    anchor_ply: u32,
    anchor_kind: String,
    entry_side: char,
    /// アンカー手を探索した局面の先手視点評価値。
    anchor_move_eval_cp_black: Option<i32>,
    total_plies: u32,
    source_year: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Partition,
    Dedup,
    Finalize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunState {
    version: u32,
    manifest: String,
    partitions: usize,
    phase: Phase,
    /// 先頭から処理済みの manifest 行数（コメント・空行を含む）。
    processed_manifest_lines: usize,
    /// 処理済み prefix を `line + "\n"` で連結した SHA-256。
    processed_prefix_sha256: String,
    candidates_written: u64,
    /// 最後のcheckpointでdurableだった各partitionのbyte長。
    partition_bytes: Vec<u64>,
    next_partition: usize,
    unique_written: u64,
    startpos_bytes: u64,
    provenance_bytes: u64,
}

struct RunOptions {
    partitions: usize,
    resume: bool,
    checkpoint_interval: usize,
}

fn parse_partition_count(value: &str) -> std::result::Result<usize, String> {
    let value = parse_positive_usize(value)?;
    if value > 256 {
        return Err("must be <= 256".to_string());
    }
    Ok(value)
}

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    let value: usize = value.parse().map_err(|_| "must be an integer".to_string())?;
    if value == 0 {
        return Err("must be positive".to_string());
    }
    Ok(value)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run_with_options(
        &cli.manifest,
        &cli.out_dir,
        RunOptions {
            partitions: cli.partitions,
            resume: cli.resume,
            checkpoint_interval: cli.checkpoint_interval,
        },
    )
}

fn run_with_options(manifest: &Path, out_dir: &Path, options: RunOptions) -> Result<()> {
    ensure!(options.partitions > 0, "partitions must be positive");
    ensure!(options.checkpoint_interval > 0, "checkpoint interval must be positive");
    if path_entry_exists(out_dir)? {
        bail!("output directory already exists: {}", out_dir.display());
    }
    let parent = out_dir.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    let manifest = manifest
        .canonicalize()
        .with_context(|| format!("failed to canonicalize manifest {}", manifest.display()))?;
    let work_dir = work_dir_for(out_dir)?;
    let state_path = work_dir.join("state.json");
    let partitions_dir = work_dir.join("partitions");

    let mut state = if path_entry_exists(&work_dir)? {
        if !options.resume {
            bail!(
                "work directory already exists: {} (use --resume or remove it)",
                work_dir.display()
            );
        }
        let resume_path = if state_path.is_file() {
            state_path.clone()
        } else {
            work_dir.join("run-meta.json")
        };
        let state: RunState = serde_json::from_reader(BufReader::new(
            File::open(&resume_path)
                .with_context(|| format!("failed to open {}", resume_path.display()))?,
        ))
        .with_context(|| format!("failed to parse {}", resume_path.display()))?;
        validate_resume_state(&state, &manifest, options.partitions)?;
        verify_manifest_for_resume(&manifest, &state)?;
        eprintln!(
            "Resuming: phase={:?}, manifest_lines={}, next_partition={}, unique={}",
            state.phase, state.processed_manifest_lines, state.next_partition, state.unique_written
        );
        state
    } else {
        if options.resume {
            bail!("--resume requested but work directory does not exist: {}", work_dir.display());
        }
        fs::create_dir(&work_dir)
            .with_context(|| format!("failed to create {}", work_dir.display()))?;
        sync_directory(parent)?;
        fs::create_dir(&partitions_dir)
            .with_context(|| format!("failed to create {}", partitions_dir.display()))?;
        sync_directory(&work_dir)?;
        let state = RunState {
            version: STATE_VERSION,
            manifest: manifest.display().to_string(),
            partitions: options.partitions,
            phase: Phase::Partition,
            processed_manifest_lines: 0,
            processed_prefix_sha256: empty_sha256(),
            candidates_written: 0,
            partition_bytes: vec![0; options.partitions],
            next_partition: 0,
            unique_written: 0,
            startpos_bytes: 0,
            provenance_bytes: 0,
        };
        save_state(&state_path, &state)?;
        state
    };

    match state.phase {
        Phase::Partition => {
            partition_manifest(
                &manifest,
                &work_dir,
                &partitions_dir,
                &state_path,
                &mut state,
                options.checkpoint_interval,
            )?;
            deduplicate_partitions(&work_dir, &partitions_dir, &state_path, &mut state)?;
        }
        Phase::Dedup => {
            deduplicate_partitions(&work_dir, &partitions_dir, &state_path, &mut state)?;
        }
        Phase::Finalize => {}
    }

    if state.unique_written == 0 {
        fs::remove_dir_all(&work_dir).ok();
        bail!("no start positions extracted from manifest {}", manifest.display());
    }
    finalize_output(&work_dir, out_dir, &state)?;
    eprintln!(
        "Done: {} unique start positions ({} candidates)",
        state.unique_written, state.candidates_written
    );
    Ok(())
}

fn validate_resume_state(state: &RunState, manifest: &Path, partitions: usize) -> Result<()> {
    ensure!(state.version == STATE_VERSION, "unsupported state version: {}", state.version);
    ensure!(
        state.manifest == manifest.display().to_string(),
        "manifest path does not match state"
    );
    ensure!(state.partitions == partitions, "--partitions does not match state");
    ensure!(
        state.partition_bytes.len() == partitions,
        "partition checkpoint count does not match state"
    );
    Ok(())
}

fn verify_manifest_for_resume(manifest: &Path, state: &RunState) -> Result<()> {
    let file = File::open(manifest)
        .with_context(|| format!("failed to open manifest {}", manifest.display()))?;
    let mut hasher = Sha256::new();
    let mut total_lines = 0usize;
    for (line_idx, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line_idx < state.processed_manifest_lines {
            update_prefix_hash(&mut hasher, &line);
        }
        total_lines = line_idx + 1;
    }
    ensure!(
        total_lines >= state.processed_manifest_lines,
        "manifest is shorter than the processed prefix recorded in state"
    );
    ensure!(
        hex_digest(hasher.finalize()) == state.processed_prefix_sha256,
        "manifest processed prefix changed; restart from a new out-dir"
    );
    if state.phase != Phase::Partition {
        ensure!(
            total_lines == state.processed_manifest_lines,
            "manifest changed after partitioning completed; restart from a new out-dir"
        );
    }
    Ok(())
}

fn partition_manifest(
    manifest: &Path,
    work_dir: &Path,
    partitions_dir: &Path,
    state_path: &Path,
    state: &mut RunState,
    checkpoint_interval: usize,
) -> Result<()> {
    for partition in 0..state.partitions {
        truncate_partition_to_checkpoint(
            &partition_path(partitions_dir, partition),
            state.partition_bytes[partition],
        )?;
    }
    let mut writers =
        PartitionWriters::new(partitions_dir, state.partitions, MAX_OPEN_PARTITION_WRITERS)?;
    let file = File::open(manifest)
        .with_context(|| format!("failed to open manifest {}", manifest.display()))?;
    let manifest_dir = manifest.parent().unwrap_or(Path::new("."));
    let mut prefix_hasher = Sha256::new();
    let mut lines_since_checkpoint = 0usize;

    for (line_idx, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        update_prefix_hash(&mut prefix_hasher, &line);
        if line_idx < state.processed_manifest_lines {
            if line_idx + 1 == state.processed_manifest_lines {
                let actual = hex_digest(prefix_hasher.clone().finalize());
                ensure!(
                    actual == state.processed_prefix_sha256,
                    "manifest processed prefix changed; remove {} and restart",
                    work_dir.display()
                );
            }
            continue;
        }

        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            let mut row = parse_manifest_row(trimmed)
                .with_context(|| format!("invalid manifest line {}", line_idx + 1))?;
            if row.csa_path.is_relative() {
                row.csa_path = manifest_dir.join(&row.csa_path);
            }
            for item in extract_from_row(&row)? {
                let partition =
                    stable_hash(item.position_key.as_bytes()) as usize % state.partitions;
                writers.write_record(partition, &item)?;
                state.candidates_written += 1;
            }
        }

        state.processed_manifest_lines = line_idx + 1;
        state.processed_prefix_sha256 = hex_digest(prefix_hasher.clone().finalize());
        lines_since_checkpoint += 1;
        if lines_since_checkpoint >= checkpoint_interval {
            writers.checkpoint()?;
            update_partition_checkpoint(partitions_dir, state)?;
            save_state(state_path, state)?;
            eprintln!(
                "partition: {} manifest lines, {} candidates",
                state.processed_manifest_lines, state.candidates_written
            );
            lines_since_checkpoint = 0;
        }
    }

    if state.processed_manifest_lines > 0 {
        let actual = hex_digest(prefix_hasher.finalize());
        ensure!(
            actual == state.processed_prefix_sha256,
            "manifest is shorter than the processed prefix recorded in state"
        );
    }
    writers.checkpoint()?;
    update_partition_checkpoint(partitions_dir, state)?;
    state.phase = Phase::Dedup;
    state.next_partition = 0;
    initialize_output_files(work_dir, state)?;
    save_state(state_path, state)?;
    Ok(())
}

fn deduplicate_partitions(
    work_dir: &Path,
    partitions_dir: &Path,
    state_path: &Path,
    state: &mut RunState,
) -> Result<()> {
    let startpos_path = work_dir.join("startpos.txt.tmp");
    let provenance_path = work_dir.join("provenance.tsv.tmp");
    truncate_to_checkpoint(&startpos_path, state.startpos_bytes)?;
    truncate_to_checkpoint(&provenance_path, state.provenance_bytes)?;
    let mut startpos = BufWriter::new(OpenOptions::new().append(true).open(&startpos_path)?);
    let mut provenance = BufWriter::new(OpenOptions::new().append(true).open(&provenance_path)?);

    for partition in state.next_partition..state.partitions {
        let path = partition_path(partitions_dir, partition);
        truncate_partial_last_line(&path)?;
        let file =
            File::open(&path).with_context(|| format!("failed to open {}", path.display()))?;
        let mut seen = HashSet::new();
        for (line_idx, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            let item: ExtractedPosition = serde_json::from_str(&line).with_context(|| {
                format!("invalid partition record {}:{}", path.display(), line_idx + 1)
            })?;
            if !seen.insert(item.position_key.clone()) {
                continue;
            }
            state.unique_written += 1;
            writeln!(startpos, "position sfen {}", item.sfen)?;
            writeln!(
                provenance,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                state.unique_written,
                state.unique_written,
                item.source_csa.display(),
                item.anchor_ply,
                item.anchor_kind,
                item.entry_side,
                item.anchor_move_eval_cp_black.map_or_else(String::new, |v| v.to_string()),
                item.total_plies,
                item.source_year.map_or_else(String::new, |v| v.to_string()),
            )?;
        }
        drop(seen);
        startpos.flush()?;
        provenance.flush()?;
        startpos.get_ref().sync_data()?;
        provenance.get_ref().sync_data()?;
        state.next_partition = partition + 1;
        state.startpos_bytes = fs::metadata(&startpos_path)?.len();
        state.provenance_bytes = fs::metadata(&provenance_path)?.len();
        save_state(state_path, state)?;
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove completed partition {}", path.display()))?;
        sync_directory(partitions_dir)?;
        if partition.is_multiple_of(16) || partition + 1 == state.partitions {
            eprintln!(
                "dedup: partition {}/{}, {} unique",
                partition + 1,
                state.partitions,
                state.unique_written
            );
        }
    }
    state.phase = Phase::Finalize;
    save_state(state_path, state)?;
    Ok(())
}

fn initialize_output_files(work_dir: &Path, state: &mut RunState) -> Result<()> {
    let startpos_path = work_dir.join("startpos.txt.tmp");
    let provenance_path = work_dir.join("provenance.tsv.tmp");
    let startpos = File::create(&startpos_path)?;
    startpos.sync_data()?;
    let mut provenance = BufWriter::new(File::create(&provenance_path)?);
    writeln!(
        provenance,
        "id\tstartpos_line\tsource_csa\tanchor_ply\tanchor_kind\tentry_side\tanchor_move_eval_cp_black\ttotal_plies\tsource_year"
    )?;
    provenance.flush()?;
    provenance.get_ref().sync_data()?;
    sync_directory(work_dir)?;
    state.startpos_bytes = 0;
    state.provenance_bytes = fs::metadata(provenance_path)?.len();
    Ok(())
}

fn finalize_output(work_dir: &Path, out_dir: &Path, state: &RunState) -> Result<()> {
    ensure!(state.next_partition == state.partitions, "dedup is not complete");
    let startpos_tmp = work_dir.join("startpos.txt.tmp");
    let provenance_tmp = work_dir.join("provenance.tsv.tmp");
    publish_staged_file(&startpos_tmp, &work_dir.join("startpos.txt"))?;
    publish_staged_file(&provenance_tmp, &work_dir.join("provenance.tsv"))?;
    sync_directory(work_dir)?;
    if work_dir.join("partitions").exists() {
        fs::remove_dir_all(work_dir.join("partitions"))?;
        sync_directory(work_dir)?;
    }
    let meta = serde_json::to_string_pretty(state)?;
    write_atomic_durable(&work_dir.join("run-meta.json"), &(meta + "\n"))?;
    if work_dir.join("state.json").exists() {
        fs::remove_file(work_dir.join("state.json"))?;
        sync_directory(work_dir)?;
    }
    rename_noreplace(work_dir, out_dir).with_context(|| {
        format!("failed to publish {} as {}", work_dir.display(), out_dir.display())
    })?;
    sync_directory(out_dir.parent().unwrap_or(Path::new(".")))?;
    Ok(())
}

fn publish_staged_file(staged: &Path, final_path: &Path) -> Result<()> {
    if staged.exists() {
        File::open(staged)?.sync_all()?;
        fs::rename(staged, final_path)?;
    } else {
        ensure!(final_path.is_file(), "missing staged output: {}", staged.display());
    }
    Ok(())
}

fn parse_manifest_row(line: &str) -> Result<ManifestRow> {
    let cols: Vec<&str> = line.split('\t').collect();
    if cols.len() != 4 {
        bail!("manifest row must have 4 tab-separated columns");
    }
    Ok(ManifestRow {
        csa_path: PathBuf::from(cols[0]),
        black_entry_ply: cols[1].parse().context("invalid black_entry_ply")?,
        white_entry_ply: cols[2].parse().context("invalid white_entry_ply")?,
        total_plies: cols[3].parse().context("invalid total_plies")?,
    })
}

fn anchor_candidates(row: &ManifestRow) -> Vec<AnchorCandidate> {
    let mut out = Vec::new();
    append_anchor_candidates(&mut out, row.black_entry_ply, 'b', row.total_plies);
    append_anchor_candidates(&mut out, row.white_entry_ply, 'w', row.total_plies);
    out
}

fn append_anchor_candidates(
    out: &mut Vec<AnchorCandidate>,
    entry_ply: i32,
    entry_side: char,
    total_plies: u32,
) {
    if entry_ply <= 0 {
        return;
    }
    const OFFSETS: [(i32, &str); 4] = [
        (-40, "entry-40"),
        (-20, "entry-20"),
        (0, "entry"),
        (20, "entry+20"),
    ];
    let max_anchor = i64::from(total_plies) - 8;
    for (offset, anchor_kind) in OFFSETS {
        let anchor = i64::from(entry_ply) + i64::from(offset);
        if anchor < 16 || anchor > max_anchor {
            continue;
        }
        out.push(AnchorCandidate {
            anchor_ply: anchor as u32,
            anchor_kind,
            entry_side,
        });
    }
}

fn extract_from_row(row: &ManifestRow) -> Result<Vec<ExtractedPosition>> {
    let candidates = anchor_candidates(row);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let text = fs::read_to_string(&row.csa_path)
        .with_context(|| format!("failed to read {}", row.csa_path.display()))?;
    let (initial_pos, parsed, _info) = parse_csa_full(&text)
        .with_context(|| format!("failed to parse {}", row.csa_path.display()))?;
    let normal_moves: Vec<_> = parsed
        .iter()
        .filter_map(|pm| match pm {
            ParsedMove::Normal(cm) => Some(cm),
            ParsedMove::Special(_) => None,
        })
        .collect();

    let mut out = Vec::new();
    for candidate in candidates {
        let anchor_idx = candidate.anchor_ply as usize;
        if anchor_idx > normal_moves.len() {
            bail!(
                "{}: anchor_ply {} exceeds normal move count {}",
                row.csa_path.display(),
                candidate.anchor_ply,
                normal_moves.len()
            );
        }

        let mut pos = initial_pos.clone();
        for cm in normal_moves.iter().take(anchor_idx) {
            pos.apply_csa_move(&cm.mv).with_context(|| {
                format!(
                    "{}: failed to replay move {} for anchor {}",
                    row.csa_path.display(),
                    cm.mv,
                    candidate.anchor_ply
                )
            })?;
        }

        let sfen = pos.to_sfen();
        if is_declarable_for_side_to_move(&sfen)? {
            continue;
        }
        out.push(ExtractedPosition {
            position_key: position_key(&sfen)?,
            sfen,
            source_csa: row.csa_path.clone(),
            anchor_ply: candidate.anchor_ply,
            anchor_kind: candidate.anchor_kind.to_string(),
            entry_side: candidate.entry_side,
            anchor_move_eval_cp_black: normal_moves[anchor_idx - 1].eval_cp_black,
            total_plies: row.total_plies,
            source_year: extract_source_year(&row.csa_path),
        });
    }
    Ok(out)
}

fn position_key(sfen: &str) -> Result<String> {
    let mut tokens = sfen.split_whitespace();
    let board = tokens.next().context("SFEN has no board")?;
    let side = tokens.next().context("SFEN has no side-to-move")?;
    let hand = tokens.next().context("SFEN has no hand")?;
    ensure!(tokens.next().is_some(), "SFEN has no move count");
    Ok(format!("{board} {side} {hand}"))
}

fn is_declarable_for_side_to_move(sfen: &str) -> Result<bool> {
    let mut pos = CorePosition::new();
    pos.set_sfen(sfen)
        .map_err(|e| anyhow!("invalid SFEN after CSA replay: {e:?}: {sfen}"))?;
    Ok(pos.declaration_win(EnteringKingRule::Point27) != Move::NONE)
}

fn extract_source_year(path: &Path) -> Option<u16> {
    path.components().find_map(|component| {
        let s = component.as_os_str().to_str()?;
        if s.len() == 4
            && s.as_bytes().iter().all(u8::is_ascii_digit)
            && let Ok(year) = s.parse::<u16>()
            && (1900..=2100).contains(&year)
        {
            return Some(year);
        }
        None
    })
}

fn work_dir_for(out_dir: &Path) -> Result<PathBuf> {
    let name = out_dir
        .file_name()
        .ok_or_else(|| anyhow!("out-dir must have a final path component"))?;
    let mut work_name = name.to_os_string();
    work_name.push(".work");
    Ok(out_dir.with_file_name(work_name))
}

fn partition_path(dir: &Path, partition: usize) -> PathBuf {
    dir.join(format!("partition_{partition:04}.jsonl"))
}

struct CachedPartitionWriter {
    writer: BufWriter<File>,
    last_used: u64,
}

struct PartitionWriters {
    dir: PathBuf,
    slots: Vec<Option<CachedPartitionWriter>>,
    dirty: Vec<bool>,
    max_open: usize,
    open_count: usize,
    tick: u64,
}

impl PartitionWriters {
    fn new(dir: &Path, count: usize, max_open: usize) -> Result<Self> {
        ensure!(max_open > 0, "max_open must be positive");
        for partition in 0..count {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(partition_path(dir, partition))?;
        }
        sync_directory(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            slots: (0..count).map(|_| None).collect(),
            dirty: vec![false; count],
            max_open: max_open.min(count.max(1)),
            open_count: 0,
            tick: 0,
        })
    }

    fn write_record(&mut self, partition: usize, item: &ExtractedPosition) -> Result<()> {
        self.ensure_open(partition)?;
        let slot = self.slots[partition].as_mut().expect("partition writer was opened");
        serde_json::to_writer(&mut slot.writer, item)?;
        slot.writer.write_all(b"\n")?;
        self.dirty[partition] = true;
        Ok(())
    }

    fn ensure_open(&mut self, partition: usize) -> Result<()> {
        ensure!(partition < self.slots.len(), "partition index out of range");
        self.tick = self.tick.wrapping_add(1);
        if let Some(slot) = self.slots[partition].as_mut() {
            slot.last_used = self.tick;
            return Ok(());
        }
        if self.open_count >= self.max_open {
            let evict = self
                .slots
                .iter()
                .enumerate()
                .filter_map(|(idx, slot)| slot.as_ref().map(|slot| (idx, slot.last_used)))
                .min_by_key(|&(_, last_used)| last_used)
                .map(|(idx, _)| idx)
                .context("no partition writer available for eviction")?;
            let mut slot = self.slots[evict].take().expect("eviction target exists");
            slot.writer.flush()?;
            self.open_count -= 1;
        }
        let path = partition_path(&self.dir, partition);
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        self.slots[partition] = Some(CachedPartitionWriter {
            writer: BufWriter::new(file),
            last_used: self.tick,
        });
        self.open_count += 1;
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<()> {
        for (partition, slot) in self.slots.iter_mut().enumerate() {
            if self.dirty[partition]
                && let Some(slot) = slot
            {
                slot.writer.flush()?;
            }
        }
        for partition in 0..self.dirty.len() {
            if !self.dirty[partition] {
                continue;
            }
            if let Some(slot) = self.slots[partition].as_ref() {
                slot.writer.get_ref().sync_data()?;
            } else {
                OpenOptions::new()
                    .write(true)
                    .open(partition_path(&self.dir, partition))?
                    .sync_data()?;
            }
            self.dirty[partition] = false;
        }
        sync_directory(&self.dir)?;
        Ok(())
    }
}

fn truncate_partial_last_line(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    if byte[0] == b'\n' {
        return Ok(());
    }
    let mut pos = len - 1;
    while pos > 0 {
        pos -= 1;
        file.seek(SeekFrom::Start(pos))?;
        file.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            file.set_len(pos + 1)?;
            return Ok(());
        }
    }
    file.set_len(0)?;
    Ok(())
}

fn truncate_to_checkpoint(path: &Path, len: u64) -> Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    ensure!(file.metadata()?.len() >= len, "{} is shorter than checkpoint", path.display());
    file.set_len(len)?;
    Ok(())
}

fn truncate_partition_to_checkpoint(path: &Path, len: u64) -> Result<()> {
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    ensure!(file.metadata()?.len() >= len, "{} is shorter than checkpoint", path.display());
    file.set_len(len)?;
    Ok(())
}

fn update_partition_checkpoint(dir: &Path, state: &mut RunState) -> Result<()> {
    for partition in 0..state.partitions {
        state.partition_bytes[partition] = fs::metadata(partition_path(dir, partition))?.len();
    }
    Ok(())
}

fn save_state(path: &Path, state: &RunState) -> Result<()> {
    let json = serde_json::to_string_pretty(state)? + "\n";
    write_atomic_durable(path, &json)
}

fn write_atomic_durable(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create state tempfile in {}", parent.display()))?;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("failed to publish checkpoint {}", path.display()))?;
    sync_directory(parent)?;
    Ok(())
}

fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // SAFETY: CString により両パスは NUL 終端され、呼び出し中ポインタは有効。
    // flags=RENAME_NOREPLACE なので既存destinationを置換しない。
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    if path_entry_exists(destination)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", destination.display()),
        ));
    }
    fs::rename(source, destination)
}

fn update_prefix_hash(hasher: &mut Sha256, line: &str) {
    hasher.update(line.as_bytes());
    hasher.update(b"\n");
}

fn empty_sha256() -> String {
    hex_digest(Sha256::new().finalize())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cycle_moves(
        plies: usize,
        black_a: &str,
        black_b: &str,
        white_a: &str,
        white_b: &str,
    ) -> String {
        let mut text = String::new();
        for ply in 1..=plies {
            let mv = match (ply % 2 == 1, ply.div_ceil(2) % 2 == 1) {
                (true, true) => black_a,
                (true, false) => black_b,
                (false, true) => white_a,
                (false, false) => white_b,
            };
            text.push_str(mv);
            text.push('\n');
            text.push_str(&format!("'** {ply}\n"));
        }
        text
    }

    fn simple_board() -> String {
        [
            "P1 *  *  *  * -OU *  *  *  *",
            "P2 *  *  *  *  *  *  *  *  *",
            "P3 *  *  *  *  *  *  *  *  *",
            "P4 *  *  *  *  *  *  *  *  *",
            "P5 *  * +KI *  *  *  *  *  *",
            "P6 *  *  *  *  *  *  *  *  *",
            "P7 *  *  *  *  *  *  *  *  *",
            "P8 *  *  *  *  *  *  *  *  *",
            "P9 *  *  *  * +OU *  *  * -KY",
            "+",
        ]
        .join("\n")
    }

    fn declarable_board() -> String {
        [
            "P1+OU+KI+KI *  *  *  *  *  *",
            "P2+GI+GI *  *  *  *  *  *  *",
            "P3+FU+FU+FU+FU+FU+FU *  *  *",
            "P4 *  *  *  *  *  *  *  *  *",
            "P5 *  *  *  *  *  *  *  *  *",
            "P6 *  *  *  *  *  *  *  *  *",
            "P7 *  * -FU-FU-FU-FU-FU-FU *",
            "P8 * -GI-GI * -KI-KI * -KE-KY",
            "P9 *  *  *  * -OU *  * -KE-KY",
            "P+00HI00HI00KA00KA",
            "P-00FU00FU00FU",
            "+",
        ]
        .join("\n")
    }

    fn write_game(path: &Path, board: &str, plies: usize) {
        let mut text = board.to_string();
        text.push('\n');
        text.push_str(&cycle_moves(plies, "+7565KI", "+6575KI", "-1939KY", "-3919KY"));
        text.push_str("%TORYO\n");
        fs::write(path, text).unwrap();
    }

    fn test_options(resume: bool) -> RunOptions {
        RunOptions {
            partitions: 4,
            resume,
            checkpoint_interval: 1,
        }
    }

    #[test]
    fn anchor_candidates_clip_by_bounds() {
        let row = ManifestRow {
            csa_path: PathBuf::from("a.csa"),
            black_entry_ply: 20,
            white_entry_ply: -1,
            total_plies: 50,
        };
        assert_eq!(
            anchor_candidates(&row),
            vec![
                AnchorCandidate {
                    anchor_ply: 20,
                    anchor_kind: "entry",
                    entry_side: 'b',
                },
                AnchorCandidate {
                    anchor_ply: 40,
                    anchor_kind: "entry+20",
                    entry_side: 'b',
                },
            ]
        );
    }

    #[test]
    fn run_extracts_filters_declarable_and_dedups_without_move_count() {
        let dir = tempfile::tempdir().unwrap();
        let year_dir = dir.path().join("2024");
        fs::create_dir(&year_dir).unwrap();
        write_game(&year_dir.join("normal.csa"), &simple_board(), 60);

        let mut kachi = declarable_board();
        kachi.push('\n');
        kachi.push_str(&cycle_moves(40, "+7161KI", "+6171KI", "-1939KY", "-3919KY"));
        kachi.push_str("%KACHI\n");
        fs::write(year_dir.join("kachi.csa"), kachi).unwrap();

        let manifest = dir.path().join("manifest.tsv");
        fs::write(&manifest, "2024/normal.csa\t20\t40\t60\n2024/kachi.csa\t20\t-1\t30\n").unwrap();
        let out = dir.path().join("out");
        run_with_options(&manifest, &out, test_options(false)).unwrap();

        let startpos = fs::read_to_string(out.join("startpos.txt")).unwrap();
        // 20手目と40手目は同じ盤面へ戻るため、手数を除いたキーで1件になる。
        assert_eq!(startpos.lines().count(), 1);
        let provenance = fs::read_to_string(out.join("provenance.tsv")).unwrap();
        let rows: Vec<_> = provenance.lines().collect();
        assert_eq!(rows.len(), 2);
        let cols: Vec<_> = rows[1].split('\t').collect();
        assert_eq!(cols[3], "20");
        assert_eq!(cols[6], "20");
        assert_eq!(cols[8], "2024");
        assert!(out.join("run-meta.json").is_file());
        assert!(!work_dir_for(&out).unwrap().exists());

        let out2 = dir.path().join("out2");
        run_with_options(&manifest, &out2, test_options(false)).unwrap();
        for name in ["startpos.txt", "provenance.tsv", "run-meta.json"] {
            assert_eq!(fs::read(out.join(name)).unwrap(), fs::read(out2.join(name)).unwrap());
        }
    }

    #[test]
    fn failed_run_keeps_final_output_absent_and_can_resume_after_tail_fix() {
        let dir = tempfile::tempdir().unwrap();
        write_game(&dir.path().join("a.csa"), &simple_board(), 40);
        let manifest = dir.path().join("manifest.tsv");
        fs::write(&manifest, "a.csa\t20\t-1\t30\nbad-row\n").unwrap();
        let out = dir.path().join("out");
        let err = run_with_options(
            &manifest,
            &out,
            RunOptions {
                partitions: 4,
                resume: false,
                checkpoint_interval: 10,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid manifest line 2"));
        assert!(!out.exists());
        assert!(work_dir_for(&out).unwrap().join("state.json").is_file());

        // checkpoint より後の末尾を修正でき、未checkpointのpartition書き込みは切り戻される。
        fs::write(&manifest, "a.csa\t20\t-1\t30\n# fixed\n").unwrap();
        run_with_options(
            &manifest,
            &out,
            RunOptions {
                partitions: 4,
                resume: true,
                checkpoint_interval: 10,
            },
        )
        .unwrap();
        assert!(out.join("startpos.txt").is_file());
        assert_eq!(fs::read_to_string(out.join("startpos.txt")).unwrap().lines().count(), 1);
    }

    #[test]
    fn existing_output_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.tsv");
        fs::write(&manifest, "# empty\n").unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("startpos.txt"), "known-good\n").unwrap();
        let err = run_with_options(&manifest, &out, test_options(false)).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(fs::read_to_string(out.join("startpos.txt")).unwrap(), "known-good\n");
    }

    #[test]
    fn empty_extraction_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.tsv");
        fs::write(&manifest, "# comment only\n\n").unwrap();
        let out = dir.path().join("out");
        let err = run_with_options(&manifest, &out, test_options(false)).unwrap_err();
        assert!(err.to_string().contains("no start positions extracted"));
        assert!(!out.exists());
        assert!(!work_dir_for(&out).unwrap().exists());
    }

    #[test]
    fn partial_partition_record_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        fs::write(&path, b"complete\npartial").unwrap();
        truncate_partial_last_line(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete\n");
    }

    #[test]
    fn resume_rejects_manifest_changes_after_partitioning() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.tsv");
        fs::write(&manifest, "one\n").unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"one\n");
        let state = RunState {
            version: STATE_VERSION,
            manifest: manifest.display().to_string(),
            partitions: 1,
            phase: Phase::Dedup,
            processed_manifest_lines: 1,
            processed_prefix_sha256: format!("{:x}", hasher.finalize()),
            candidates_written: 0,
            partition_bytes: vec![0],
            next_partition: 0,
            unique_written: 0,
            startpos_bytes: 0,
            provenance_bytes: 0,
        };

        verify_manifest_for_resume(&manifest, &state).unwrap();
        fs::write(&manifest, "changed\n").unwrap();
        assert!(verify_manifest_for_resume(&manifest, &state).is_err());
        fs::write(&manifest, "one\nappended\n").unwrap();
        assert!(verify_manifest_for_resume(&manifest, &state).is_err());
    }

    #[test]
    fn partition_writer_cache_bounds_open_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut writers = PartitionWriters::new(dir.path(), 128, 4).unwrap();
        for partition in 0..128 {
            writers.ensure_open(partition).unwrap();
            assert!(writers.open_count <= 4);
        }
    }

    #[test]
    fn publish_never_replaces_an_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();

        assert!(rename_noreplace(&source, &destination).is_err());
        assert!(source.is_dir());
        assert!(destination.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_output_is_treated_as_existing() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.tsv");
        fs::write(&manifest, "# empty\n").unwrap();
        let out = dir.path().join("out");
        symlink(dir.path().join("missing"), &out).unwrap();

        let err = run_with_options(&manifest, &out, test_options(false)).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn finalize_recovers_after_one_staged_file_was_already_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("out.work");
        let out = dir.path().join("out");
        fs::create_dir(&work).unwrap();
        fs::create_dir(work.join("partitions")).unwrap();
        fs::write(work.join("startpos.txt"), "position sfen x\n").unwrap();
        fs::write(work.join("provenance.tsv.tmp"), "header\n").unwrap();
        fs::write(work.join("state.json"), "checkpoint\n").unwrap();
        let state = RunState {
            version: STATE_VERSION,
            manifest: "manifest.tsv".to_string(),
            partitions: 1,
            phase: Phase::Finalize,
            processed_manifest_lines: 1,
            processed_prefix_sha256: empty_sha256(),
            candidates_written: 1,
            partition_bytes: vec![0],
            next_partition: 1,
            unique_written: 1,
            startpos_bytes: 16,
            provenance_bytes: 7,
        };
        finalize_output(&work, &out, &state).unwrap();
        assert!(out.join("startpos.txt").is_file());
        assert!(out.join("provenance.tsv").is_file());
        assert!(out.join("run-meta.json").is_file());
        assert!(!out.join("state.json").exists());
    }
}
