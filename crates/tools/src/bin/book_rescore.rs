use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use crossbeam_channel as chan;
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};
use serde::{Deserialize, Serialize};
use tools::selfplay::{EngineConfig, EngineProcess};

const BOOK_HEADER: &str = "#YANEURAOU-DB2016 1.00";
const MATE_CAP: i32 = 30_000;

#[derive(Parser, Debug)]
#[command(about = "YANEURAOU-DB2016 テキスト定跡に USI 探索評価値を付与する")]
struct Cli {
    #[arg(long)]
    book: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    engine: PathBuf,
    #[arg(long = "engine-option", num_args = 1)]
    engine_options: Vec<String>,
    #[arg(long, default_value = "nodes 100000")]
    go: String,
    #[arg(long)]
    journal: PathBuf,
    #[arg(long, default_value_t = false)]
    resume: bool,
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(long, default_value_t = 1)]
    parallel: usize,
    #[arg(long = "no-parent-search", action = clap::ArgAction::SetTrue)]
    no_parent_search: bool,
}

#[derive(Debug, Clone)]
struct BookDb {
    entries: BTreeMap<String, PositionEntry>,
}

#[derive(Debug, Clone)]
struct PositionEntry {
    sfen: String,
    side: Color,
    moves: Vec<BookMove>,
}

#[derive(Debug, Clone)]
struct BookMove {
    move_usi: Option<String>,
    ponder_usi: Option<String>,
    value: i32,
    depth: i32,
    count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalKind {
    Child,
    Parent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalRecord {
    kind: JournalKind,
    sfen: String,
    go: String,
    #[serde(default)]
    engine_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bestmove: Option<String>,
}

#[derive(Debug, Clone)]
struct EvalRecord {
    value: i32,
    depth: i32,
}

#[derive(Debug, Clone)]
struct ParentRecord {
    bestmove: Option<String>,
}

#[derive(Debug, Clone)]
struct SearchTask {
    kind: JournalKind,
    key: String,
    position_tail: String,
}

#[derive(Debug, Clone)]
struct SearchResult {
    record: JournalRecord,
}

#[derive(Debug)]
enum WorkerMessage {
    Task(Result<SearchResult>),
    Fatal(anyhow::Error),
}

#[derive(Debug, Default, Clone)]
struct LoadedJournal {
    child: HashMap<String, EvalRecord>,
    parent: HashMap<String, ParentRecord>,
}

#[derive(Debug, Default, Clone)]
struct ReportStats {
    parent_total: u64,
    best_in_book: u64,
    best_is_count_top: u64,
    move_total: u64,
    gap_ge_100: u64,
    gap_ge_200: u64,
    gap_ge_300: u64,
}

impl ReportStats {
    fn add_parent(&mut self, in_book: bool, count_top: bool) {
        self.parent_total += 1;
        self.best_in_book += u64::from(in_book);
        self.best_is_count_top += u64::from(count_top);
    }

    fn add_gap(&mut self, gap: i32) {
        self.move_total += 1;
        self.gap_ge_100 += u64::from(gap >= 100);
        self.gap_ge_200 += u64::from(gap >= 200);
        self.gap_ge_300 += u64::from(gap >= 300);
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.parallel == 0 {
        bail!("--parallel は 1 以上を指定してください");
    }

    rshogi_book::Book::from_path(&cli.book, true)
        .with_context(|| format!("定跡を rshogi-book で読めません: {}", cli.book.display()))?;
    let book = read_book_db(&cli.book)?;
    let engine_fingerprint = engine_fingerprint(&cli.engine, &cli.engine_options);
    let mut journal = if cli.resume {
        load_journal(&cli.journal, &cli.go, &engine_fingerprint)?
    } else {
        LoadedJournal::default()
    };

    let tasks = build_tasks(&book, &journal, !cli.no_parent_search)?;
    if !tasks.is_empty() {
        run_search_tasks(&cli, tasks, &engine_fingerprint, &mut journal)?;
    }

    write_rescored_book(&book, &journal.child, &cli.out)?;
    if let Some(path) = &cli.report {
        write_report(&book, &journal.child, &journal.parent, path)?;
    }
    Ok(())
}

fn read_book_db(path: &Path) -> Result<BookDb> {
    let file = File::open(path).with_context(|| format!("定跡を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = BTreeMap::<String, PositionEntry>::new();
    let mut current_key: Option<String> = None;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("sfen ") {
            let sfen = rest.trim().to_string();
            let key = sfen.clone();
            let side = side_to_move(&sfen)?;
            entries.entry(key.clone()).or_insert(PositionEntry {
                sfen,
                side,
                moves: Vec::new(),
            });
            current_key = Some(key);
            continue;
        }
        let Some(key) = current_key.as_ref() else {
            continue;
        };
        if let Some(book_move) = parse_move_line(line) {
            entries
                .get_mut(key)
                .ok_or_else(|| anyhow!("内部エラー: current_key の entry がありません"))?
                .moves
                .push(book_move);
        }
    }

    Ok(BookDb { entries })
}

fn parse_move_line(line: &str) -> Option<BookMove> {
    let mut tokens = line.split_whitespace();
    let move_usi = tokens.next().map(parse_move_field)?;
    let ponder_usi = tokens.next().map(parse_move_field).unwrap_or(None);
    let value = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let depth = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let count = tokens.next().map_or(1, |t| t.parse::<u64>().unwrap_or(0));
    Some(BookMove {
        move_usi,
        ponder_usi,
        value,
        depth,
        count,
    })
}

fn parse_move_field(token: &str) -> Option<String> {
    match token {
        "none" | "None" | "resign" => None,
        other => Some(other.to_string()),
    }
}

fn strip_ply(sfen: &str) -> &str {
    match sfen.rsplit_once(' ') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => sfen,
    }
}

fn engine_fingerprint(engine_path: &Path, engine_options: &[String]) -> String {
    let engine_name =
        engine_path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let mut normalized_options: Vec<&str> = engine_options.iter().map(String::as_str).collect();
    normalized_options
        .sort_by(|a, b| engine_option_key(a).cmp(engine_option_key(b)).then_with(|| a.cmp(b)));
    format!("{engine_name}\t{}", normalized_options.join("\n"))
}

fn engine_option_key(option: &str) -> &str {
    option.split_once('=').map_or(option, |(key, _)| key)
}

fn side_to_move(sfen: &str) -> Result<Color> {
    match sfen.split_whitespace().nth(1) {
        Some("b") => Ok(Color::Black),
        Some("w") => Ok(Color::White),
        other => bail!("SFEN の手番が不正です: {other:?}: {sfen}"),
    }
}

fn child_key_after_move(parent_sfen: &str, move_usi: &str) -> Result<String> {
    let mut pos = Position::new();
    pos.set_sfen(parent_sfen)
        .map_err(|e| anyhow!("親局面 SFEN が不正です: {parent_sfen}: {e}"))?;
    let decoded =
        Move::from_usi(move_usi).ok_or_else(|| anyhow!("USI 指し手が不正です: {move_usi}"))?;
    let mv = pos
        .to_move(decoded)
        .ok_or_else(|| anyhow!("局面に適用できない指し手です: {move_usi}: {parent_sfen}"))?;
    if mv == Move::NONE || !pos.pseudo_legal(mv) || !pos.is_legal(mv) {
        bail!("非合法手です: {move_usi}: {parent_sfen}");
    }
    let gives_check = pos.gives_check(mv);
    pos.do_move(mv, gives_check);
    Ok(strip_ply(&pos.to_sfen()).to_string())
}

fn build_tasks(
    book: &BookDb,
    journal: &LoadedJournal,
    parent_search: bool,
) -> Result<Vec<SearchTask>> {
    let mut tasks = Vec::new();
    let mut child_seen = HashSet::new();
    let mut parent_seen = HashSet::new();

    for entry in book.entries.values() {
        let parent_key = strip_ply(&entry.sfen);
        if parent_search
            && !journal.parent.contains_key(parent_key)
            && parent_seen.insert(parent_key.to_string())
        {
            tasks.push(SearchTask {
                kind: JournalKind::Parent,
                key: parent_key.to_string(),
                position_tail: entry.sfen.clone(),
            });
        }
        for book_move in &entry.moves {
            let Some(move_usi) = &book_move.move_usi else {
                continue;
            };
            let child_key = child_key_after_move(&entry.sfen, move_usi)?;
            if journal.child.contains_key(&child_key) || !child_seen.insert(child_key.clone()) {
                continue;
            }
            tasks.push(SearchTask {
                kind: JournalKind::Child,
                key: child_key,
                position_tail: format!("{} moves {move_usi}", entry.sfen),
            });
        }
    }
    Ok(tasks)
}

fn load_journal(path: &Path, go_args: &str, engine_fingerprint: &str) -> Result<LoadedJournal> {
    if !path.exists() {
        return Ok(LoadedJournal::default());
    }
    let file =
        File::open(path).with_context(|| format!("journal を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut loaded = LoadedJournal::default();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: JournalRecord = serde_json::from_str(&line).with_context(|| {
            format!("journal JSON が不正です: {}:{}", path.display(), line_no + 1)
        })?;
        if rec.go != go_args || rec.engine_fingerprint != engine_fingerprint {
            continue;
        }
        match rec.kind {
            JournalKind::Child => {
                if let (Some(value), Some(depth)) = (rec.value, rec.depth) {
                    loaded.child.insert(rec.sfen, EvalRecord { value, depth });
                }
            }
            JournalKind::Parent => {
                loaded.parent.insert(
                    rec.sfen,
                    ParentRecord {
                        bestmove: rec.bestmove,
                    },
                );
            }
        }
    }
    Ok(loaded)
}

fn run_search_tasks(
    cli: &Cli,
    tasks: Vec<SearchTask>,
    engine_fingerprint: &str,
    journal: &mut LoadedJournal,
) -> Result<()> {
    let (task_tx, task_rx) = chan::unbounded::<SearchTask>();
    let (result_tx, result_rx) = chan::unbounded::<WorkerMessage>();
    let task_count = tasks.len();
    for task in tasks {
        task_tx.send(task)?;
    }
    drop(task_tx);

    let engine_options = Arc::new(cli.engine_options.clone());
    let go_args = Arc::new(cli.go.clone());
    let engine_path = Arc::new(cli.engine.clone());
    let engine_fingerprint = Arc::new(engine_fingerprint.to_string());
    let worker_count = cli.parallel;

    let mut handles = Vec::with_capacity(worker_count);
    for worker_id in 0..worker_count {
        let task_rx = task_rx.clone();
        let result_tx = result_tx.clone();
        let engine_options = engine_options.clone();
        let go_args = go_args.clone();
        let engine_path = engine_path.clone();
        let engine_fingerprint = engine_fingerprint.clone();
        handles.push(std::thread::spawn(move || {
            worker_loop(
                worker_id,
                &engine_path,
                &engine_options,
                &go_args,
                &engine_fingerprint,
                task_rx,
                result_tx,
            );
        }));
    }
    drop(result_tx);

    let journal_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cli.journal)
        .with_context(|| format!("journal を追記オープンできません: {}", cli.journal.display()))?;
    let writer = Mutex::new(BufWriter::new(journal_file));

    let mut first_error: Option<anyhow::Error> = None;
    let mut processed_count = 0usize;
    for message in result_rx {
        match message {
            WorkerMessage::Task(Ok(search_result)) => {
                processed_count += 1;
                append_journal_record(&writer, &search_result.record)?;
                match search_result.record.kind {
                    JournalKind::Child => {
                        if let (Some(value), Some(depth)) =
                            (search_result.record.value, search_result.record.depth)
                        {
                            journal
                                .child
                                .insert(search_result.record.sfen, EvalRecord { value, depth });
                        }
                    }
                    JournalKind::Parent => {
                        journal.parent.insert(
                            search_result.record.sfen,
                            ParentRecord {
                                bestmove: search_result.record.bestmove,
                            },
                        );
                    }
                }
            }
            WorkerMessage::Task(Err(err)) => {
                processed_count += 1;
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
            WorkerMessage::Fatal(err) if first_error.is_none() => first_error = Some(err),
            WorkerMessage::Fatal(_) => {}
        }
    }

    for handle in handles {
        handle.join().map_err(|_| anyhow!("worker thread が panic しました"))?;
    }
    if processed_count < task_count {
        return Err(search_tasks_incomplete_error(
            processed_count,
            task_count,
            &cli.journal,
            first_error,
        ));
    }
    if let Some(err) = first_error {
        return Err(err.context(format!(
            "探索中にエラーが発生しました。journal には途中結果が追記済みのため --resume で再開できます: {}",
            cli.journal.display()
        )));
    }
    Ok(())
}

fn worker_loop(
    worker_id: usize,
    engine_path: &Path,
    engine_options: &[String],
    go_args: &str,
    engine_fingerprint: &str,
    task_rx: chan::Receiver<SearchTask>,
    result_tx: chan::Sender<WorkerMessage>,
) {
    let cfg = EngineConfig {
        path: engine_path.to_path_buf(),
        args: Vec::new(),
        threads: 1,
        hash_mb: 256,
        network_delay: None,
        network_delay2: None,
        minimum_thinking_time: None,
        slowmover: None,
        ponder: false,
        usi_options: engine_options.to_vec(),
    };
    let mut engine = match EngineProcess::spawn(&cfg, format!("book_rescore-{worker_id}")) {
        Ok(engine) => engine,
        Err(err) => {
            let _ = result_tx.send(WorkerMessage::Fatal(
                err.context(format!("worker {worker_id}: engine 起動に失敗しました")),
            ));
            return;
        }
    };
    let timeout = Duration::from_secs(24 * 60 * 60);
    for task in task_rx {
        let result = search_one(&mut engine, &task, go_args, engine_fingerprint, timeout)
            .with_context(|| {
                format!("worker {worker_id}: {:?} 探索に失敗しました: {}", task.kind, task.key)
            });
        if result_tx.send(WorkerMessage::Task(result)).is_err() {
            break;
        }
    }
}

fn search_tasks_incomplete_error(
    processed_count: usize,
    task_count: usize,
    journal_path: &Path,
    cause: Option<anyhow::Error>,
) -> anyhow::Error {
    let message = format!(
        "探索タスクが未完了です: {processed_count}/{task_count} 件完了。journal には途中結果が追記済みのため --resume で再開できます: {}",
        journal_path.display()
    );
    match cause {
        Some(err) => err.context(message),
        None => anyhow!(message),
    }
}

fn search_one(
    engine: &mut EngineProcess,
    task: &SearchTask,
    go_args: &str,
    engine_fingerprint: &str,
    timeout: Duration,
) -> Result<SearchResult> {
    let outcome = engine.search_raw_go(&task.position_tail, go_args, timeout, None)?;
    let eval = outcome
        .eval
        .ok_or_else(|| anyhow!("info score が得られませんでした: {}", task.key))?;
    let depth = eval.depth.map(|d| d as i32).unwrap_or(0);
    let record = match task.kind {
        JournalKind::Child => {
            let child_score = score_to_cp(eval.score_cp, eval.score_mate)
                .ok_or_else(|| anyhow!("score cp/mate が得られませんでした: {}", task.key))?;
            JournalRecord {
                kind: JournalKind::Child,
                sfen: task.key.clone(),
                go: go_args.to_string(),
                engine_fingerprint: engine_fingerprint.to_string(),
                value: Some(value_from_child_score(child_score)),
                depth: Some(depth),
                bestmove: None,
            }
        }
        JournalKind::Parent => JournalRecord {
            kind: JournalKind::Parent,
            sfen: task.key.clone(),
            go: go_args.to_string(),
            engine_fingerprint: engine_fingerprint.to_string(),
            value: None,
            depth: Some(depth),
            bestmove: outcome.bestmove,
        },
    };
    Ok(SearchResult { record })
}

fn append_journal_record(writer: &Mutex<BufWriter<File>>, rec: &JournalRecord) -> Result<()> {
    let mut guard = writer.lock().map_err(|_| anyhow!("journal writer mutex poisoned"))?;
    serde_json::to_writer(&mut *guard, rec)?;
    guard.write_all(b"\n")?;
    guard.flush()?;
    Ok(())
}

fn score_to_cp(score_cp: Option<i32>, score_mate: Option<i32>) -> Option<i32> {
    if let Some(cp) = score_cp {
        return Some(cp.clamp(-MATE_CAP, MATE_CAP));
    }
    score_mate.map(mate_to_cp)
}

fn mate_to_cp(mate: i32) -> i32 {
    let ply = mate.saturating_abs().min(MATE_CAP);
    if mate >= 0 {
        (MATE_CAP - ply).clamp(-MATE_CAP, MATE_CAP)
    } else {
        (-MATE_CAP + ply).clamp(-MATE_CAP, MATE_CAP)
    }
}

fn value_from_child_score(child_score: i32) -> i32 {
    -child_score.clamp(-MATE_CAP, MATE_CAP)
}

fn write_rescored_book(
    book: &BookDb,
    child_records: &HashMap<String, EvalRecord>,
    out: &Path,
) -> Result<()> {
    let file =
        File::create(out).with_context(|| format!("出力を作成できません: {}", out.display()))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(BOOK_HEADER.as_bytes())?;
    writer.write_all(b"\n")?;
    for entry in book.entries.values() {
        writeln!(writer, "sfen {}", entry.sfen)?;
        let mut moves = entry.moves.clone();
        moves.sort_by(|a, b| {
            b.count.cmp(&a.count).then_with(|| move_sort_key(a).cmp(move_sort_key(b)))
        });
        for book_move in &moves {
            let (value, depth) = if let Some(move_usi) = &book_move.move_usi {
                let child_key = child_key_after_move(&entry.sfen, move_usi)?;
                child_records
                    .get(&child_key)
                    .map(|r| (r.value, r.depth))
                    .unwrap_or((book_move.value, book_move.depth))
            } else {
                (book_move.value, book_move.depth)
            };
            writeln!(
                writer,
                "{} {} {} {} {}",
                book_move.move_usi.as_deref().unwrap_or("none"),
                book_move.ponder_usi.as_deref().unwrap_or("none"),
                value,
                depth,
                book_move.count
            )?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn move_sort_key(book_move: &BookMove) -> &str {
    book_move.move_usi.as_deref().unwrap_or("none")
}

fn write_report(
    book: &BookDb,
    child_records: &HashMap<String, EvalRecord>,
    parent_records: &HashMap<String, ParentRecord>,
    path: &Path,
) -> Result<()> {
    let mut all = ReportStats::default();
    let mut black = ReportStats::default();
    let mut white = ReportStats::default();

    for entry in book.entries.values() {
        if let Some(parent) = parent_records.get(strip_ply(&entry.sfen))
            && let Some(bestmove) =
                parent.bestmove.as_deref().filter(|m| *m != "none" && *m != "resign")
        {
            let candidate_set: HashSet<&str> =
                entry.moves.iter().filter_map(|m| m.move_usi.as_deref()).collect();
            let count_top = entry
                .moves
                .iter()
                .max_by(|a, b| {
                    a.count.cmp(&b.count).then_with(|| move_sort_key(b).cmp(move_sort_key(a)))
                })
                .and_then(|m| m.move_usi.as_deref());
            let in_book = candidate_set.contains(bestmove);
            let is_count_top = count_top == Some(bestmove);
            all.add_parent(in_book, is_count_top);
            side_stats_mut(entry.side, &mut black, &mut white).add_parent(in_book, is_count_top);
        }

        let mut values = Vec::new();
        for book_move in &entry.moves {
            let Some(move_usi) = &book_move.move_usi else {
                continue;
            };
            let child_key = child_key_after_move(&entry.sfen, move_usi)?;
            if let Some(record) = child_records.get(&child_key) {
                values.push(record.value);
            }
        }
        if let Some(top) = values.iter().max().copied() {
            for value in values {
                let gap = top - value;
                all.add_gap(gap);
                side_stats_mut(entry.side, &mut black, &mut white).add_gap(gap);
            }
        }
    }

    let file = File::create(path)
        .with_context(|| format!("report を作成できません: {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writeln!(writer, "section\tside\tmetric\tvalue")?;
    write_stats(&mut writer, "all", &all)?;
    write_stats(&mut writer, "black", &black)?;
    write_stats(&mut writer, "white", &white)?;
    writer.flush()?;
    Ok(())
}

fn side_stats_mut<'a>(
    side: Color,
    black: &'a mut ReportStats,
    white: &'a mut ReportStats,
) -> &'a mut ReportStats {
    match side {
        Color::Black => black,
        Color::White => white,
    }
}

fn write_stats(writer: &mut impl Write, side: &str, stats: &ReportStats) -> Result<()> {
    writeln!(writer, "parent\t{side}\ttotal\t{}", stats.parent_total)?;
    writeln!(
        writer,
        "parent\t{side}\tbestmove_in_book_rate\t{:.6}",
        ratio(stats.best_in_book, stats.parent_total)
    )?;
    writeln!(
        writer,
        "parent\t{side}\tbestmove_is_count_top_rate\t{:.6}",
        ratio(stats.best_is_count_top, stats.parent_total)
    )?;
    writeln!(writer, "move\t{side}\ttotal\t{}", stats.move_total)?;
    writeln!(writer, "move\t{side}\tgap_ge_100\t{}", stats.gap_ge_100)?;
    writeln!(writer, "move\t{side}\tgap_ge_200\t{}", stats.gap_ge_200)?;
    writeln!(writer, "move\t{side}\tgap_ge_300\t{}", stats.gap_ge_300)?;
    Ok(())
}

fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    #[test]
    fn value_uses_parent_perspective_by_negating_child_score() {
        assert_eq!(value_from_child_score(123), -123);
        assert_eq!(value_from_child_score(-456), 456);
    }

    #[test]
    fn mate_score_converts_to_capped_cp() {
        assert_eq!(mate_to_cp(1), 29_999);
        assert_eq!(mate_to_cp(12), 29_988);
        assert_eq!(mate_to_cp(-1), -29_999);
        assert_eq!(mate_to_cp(-12), -29_988);
        assert_eq!(mate_to_cp(40_000), 0);
        assert_eq!(score_to_cp(Some(40_001), None), Some(30_000));
        assert_eq!(score_to_cp(Some(-40_001), None), Some(-30_000));
    }

    #[test]
    fn db_output_from_journal_is_deterministic() {
        let mut entries = BTreeMap::new();
        entries.insert(
            START.to_string(),
            PositionEntry {
                sfen: START.to_string(),
                side: Color::Black,
                moves: vec![
                    BookMove {
                        move_usi: Some("2g2f".to_string()),
                        ponder_usi: None,
                        value: 0,
                        depth: 0,
                        count: 10,
                    },
                    BookMove {
                        move_usi: Some("7g7f".to_string()),
                        ponder_usi: Some("3c3d".to_string()),
                        value: 0,
                        depth: 0,
                        count: 10,
                    },
                    BookMove {
                        move_usi: Some("5i6h".to_string()),
                        ponder_usi: None,
                        value: 0,
                        depth: 0,
                        count: 3,
                    },
                ],
            },
        );
        let book = BookDb { entries };
        let mut child_records = HashMap::new();
        for (mv, value, depth) in [("7g7f", 100, 15), ("2g2f", -20, 16), ("5i6h", 30, 14)] {
            child_records
                .insert(child_key_after_move(START, mv).unwrap(), EvalRecord { value, depth });
        }
        let dir = tempfile::tempdir().unwrap();
        let out1 = dir.path().join("a.db");
        let out2 = dir.path().join("b.db");
        write_rescored_book(&book, &child_records, &out1).unwrap();
        write_rescored_book(&book, &child_records, &out2).unwrap();
        let a = std::fs::read_to_string(out1).unwrap();
        let b = std::fs::read_to_string(out2).unwrap();
        assert_eq!(a, b);
        assert!(a.contains("2g2f none -20 16 10\n7g7f 3c3d 100 15 10\n5i6h none 30 14 3\n"));
    }

    #[test]
    fn worker_spawn_failure_returns_error_when_tasks_remain() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            book: dir.path().join("input.db"),
            out: dir.path().join("out.db"),
            engine: dir.path().join("missing-engine"),
            engine_options: Vec::new(),
            go: "nodes 1".to_string(),
            journal: dir.path().join("journal.jsonl"),
            resume: false,
            report: None,
            parallel: 1,
            no_parent_search: false,
        };
        let tasks = vec![SearchTask {
            kind: JournalKind::Parent,
            key: strip_ply(START).to_string(),
            position_tail: START.to_string(),
        }];
        let mut journal = LoadedJournal::default();

        let fingerprint = engine_fingerprint(&cli.engine, &cli.engine_options);
        let err = run_search_tasks(&cli, tasks, &fingerprint, &mut journal).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("探索タスクが未完了です: 0/1 件完了"));
        assert!(message.contains("--resume で再開できます"));
        assert!(journal.child.is_empty());
        assert!(journal.parent.is_empty());
    }

    #[test]
    fn read_book_db_preserves_ply_distinct_sfen_rows_and_shares_child_eval() {
        let dir = tempfile::tempdir().unwrap();
        let book_path = dir.path().join("input.db");
        let out_path = dir.path().join("out.db");
        let input = format!(
            "{BOOK_HEADER}\n\
             sfen {sfen_a}\n\
             7g7f none 0 0 10\n\
             sfen {sfen_b}\n\
             7g7f none 0 0 20\n",
            sfen_a = START,
            sfen_b = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 77"
        );
        std::fs::write(&book_path, input).unwrap();

        let book = read_book_db(&book_path).unwrap();
        assert_eq!(book.entries.len(), 2);

        let tasks = build_tasks(&book, &LoadedJournal::default(), false).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, JournalKind::Child);

        let mut child_records = HashMap::new();
        child_records.insert(
            child_key_after_move(START, "7g7f").unwrap(),
            EvalRecord {
                value: 123,
                depth: 9,
            },
        );
        write_rescored_book(&book, &child_records, &out_path).unwrap();

        let output = std::fs::read_to_string(out_path).unwrap();
        let expected = format!(
            "{BOOK_HEADER}\n\
             sfen {sfen_a}\n\
             7g7f none 123 9 10\n\
             sfen {sfen_b}\n\
             7g7f none 123 9 20\n",
            sfen_a = START,
            sfen_b = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 77"
        );
        assert_eq!(output, expected);
    }

    #[test]
    fn resume_ignores_journal_record_with_mismatched_engine_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let go = "nodes 1";
        let current_engine = dir.path().join("new-engine");
        let current_options = vec!["EvalFile=current.nnue".to_string()];
        let current_fingerprint = engine_fingerprint(&current_engine, &current_options);
        let stale_record = JournalRecord {
            kind: JournalKind::Child,
            sfen: strip_ply(START).to_string(),
            go: go.to_string(),
            engine_fingerprint: engine_fingerprint(
                Path::new("/path/to/old-engine"),
                &["EvalFile=old.nnue".to_string()],
            ),
            value: Some(42),
            depth: Some(3),
            bestmove: None,
        };
        std::fs::write(
            &journal_path,
            format!("{}\n", serde_json::to_string(&stale_record).unwrap()),
        )
        .unwrap();

        let loaded = load_journal(&journal_path, go, &current_fingerprint).unwrap();
        assert!(loaded.child.is_empty());

        let matching_record = JournalRecord {
            engine_fingerprint: current_fingerprint.clone(),
            ..stale_record
        };
        std::fs::write(
            &journal_path,
            format!("{}\n", serde_json::to_string(&matching_record).unwrap()),
        )
        .unwrap();

        let loaded = load_journal(&journal_path, go, &current_fingerprint).unwrap();
        assert_eq!(loaded.child[strip_ply(START)].value, 42);
    }
}
