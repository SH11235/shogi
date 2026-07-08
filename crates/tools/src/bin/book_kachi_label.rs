//! 定跡ノードごとの入玉宣言勝ち(%KACHI)リスクを CSA corpus から集計するツール。

use anyhow::{Context, Result};
use clap::Parser;
use rayon::prelude::*;
use rshogi_csa::{Color as CsaColor, ParsedMove, SpecialMove};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tools::common::io::write_atomic;

#[derive(Parser, Debug)]
#[command(
    name = "book_kachi_label",
    version,
    about = "定跡ノード×指し手の %KACHI 決着率を CSA corpus から JSONL sidecar に集計する"
)]
struct Cli {
    /// YANEURAOU-DB2016 テキスト定跡 .db
    #[arg(long)]
    book: PathBuf,
    /// CSA corpus のルートディレクトリ。配下の *.csa を再帰走査する
    #[arg(long)]
    corpus: PathBuf,
    /// sidecar JSONL 出力先
    #[arg(long)]
    out: PathBuf,
    /// 両対局者の対局時レート下限。0 ならレートで除外しない
    #[arg(long, default_value_t = 4000)]
    min_rating: u32,
    /// Markdown report 出力先
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct BookDb {
    nodes: BTreeMap<String, BTreeSet<String>>,
    move_count: usize,
}

#[derive(Debug, Default, Clone)]
struct LabelAgg {
    games: u64,
    kachi_black: u64,
    kachi_white: u64,
}

impl LabelAgg {
    fn add_game(&mut self, kachi_side: Option<Side>) {
        self.games += 1;
        match kachi_side {
            Some(Side::Black) => self.kachi_black += 1,
            Some(Side::White) => self.kachi_white += 1,
            None => {}
        }
    }

    fn merge_from(&mut self, other: LabelAgg) {
        self.games += other.games;
        self.kachi_black += other.kachi_black;
        self.kachi_white += other.kachi_white;
    }

    fn kachi_rate(&self) -> f64 {
        if self.games == 0 {
            0.0
        } else {
            (self.kachi_black + self.kachi_white) as f64 / self.games as f64
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Stats {
    files_seen: u64,
    parse_errors: u64,
    no_rating: u64,
    rating_skipped: u64,
    replay_errors: u64,
    eligible_games: u64,
    kachi_games: u64,
    games_with_hits: u64,
    direct_hits: u64,
    flipped_hits: u64,
    node_misses: u64,
    move_misses: u64,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.files_seen += other.files_seen;
        self.parse_errors += other.parse_errors;
        self.no_rating += other.no_rating;
        self.rating_skipped += other.rating_skipped;
        self.replay_errors += other.replay_errors;
        self.eligible_games += other.eligible_games;
        self.kachi_games += other.kachi_games;
        self.games_with_hits += other.games_with_hits;
        self.direct_hits += other.direct_hits;
        self.flipped_hits += other.flipped_hits;
        self.node_misses += other.node_misses;
        self.move_misses += other.move_misses;
    }
}

#[derive(Debug, Default)]
struct Aggregator {
    labels: BTreeMap<(String, String), LabelAgg>,
    stats: Stats,
}

impl Aggregator {
    fn record(&mut self, key: String, move_usi: String, kachi_side: Option<Side>) {
        self.labels.entry((key, move_usi)).or_default().add_game(kachi_side);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Black,
    White,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Side::Black => Side::White,
            Side::White => Side::Black,
        }
    }
}

impl From<CsaColor> for Side {
    fn from(value: CsaColor) -> Self {
        match value {
            CsaColor::Black => Side::Black,
            CsaColor::White => Side::White,
        }
    }
}

#[derive(Debug)]
struct MatchedBookMove {
    key: String,
    move_usi: String,
    flipped: bool,
}

#[derive(Debug)]
struct BufferedHit {
    key: String,
    move_usi: String,
    kachi_side: Option<Side>,
    flipped: bool,
}

#[derive(Serialize)]
struct SidecarRecord<'a> {
    sfen_key: &'a str,
    #[serde(rename = "move")]
    move_usi: &'a str,
    games: u64,
    kachi_black: u64,
    kachi_white: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let book = read_book_db(&cli.book)?;
    let csa_paths = collect_csa_paths(&cli.corpus)?;
    let agg = build_labels(&book, &csa_paths, cli.min_rating);

    write_sidecar(&agg, &cli.out)?;
    if let Some(path) = &cli.report {
        write_report(&agg, &book, path)?;
    }

    eprintln!("book nodes: {}", book.nodes.len());
    eprintln!("book moves: {}", book.move_count);
    eprintln!("CSA files: {}", agg.stats.files_seen);
    eprintln!("eligible games: {}", agg.stats.eligible_games);
    eprintln!("KACHI games: {}", agg.stats.kachi_games);
    eprintln!("sidecar rows: {}", agg.labels.len());
    Ok(())
}

fn read_book_db(path: &Path) -> Result<BookDb> {
    let file = File::open(path).with_context(|| format!("定跡を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut nodes = BTreeMap::<String, BTreeSet<String>>::new();
    let mut current_key: Option<String> = None;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }

        if let Some(rest) = line.strip_prefix("sfen ") {
            let key = strip_ply(rest.trim()).to_string();
            nodes.entry(key.clone()).or_default();
            current_key = Some(key);
            continue;
        }

        let Some(key) = current_key.as_ref() else {
            continue;
        };
        if let Some(move_usi) = parse_book_move_line(line) {
            nodes.entry(key.clone()).or_default().insert(move_usi);
        }
    }

    let move_count = nodes.values().map(BTreeSet::len).sum();
    Ok(BookDb { nodes, move_count })
}

fn parse_book_move_line(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?;
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

fn collect_csa_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_csa_paths_rec(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_csa_paths_rec(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let meta = fs::symlink_metadata(path)
        .with_context(|| format!("パスのメタデータ取得に失敗: {}", path.display()))?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)
            .with_context(|| format!("ディレクトリ読込に失敗: {}", path.display()))?
        {
            let entry = entry
                .with_context(|| format!("ディレクトリエントリ読込に失敗: {}", path.display()))?;
            collect_csa_paths_rec(&entry.path(), out)?;
        }
    } else if meta.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csa"))
    {
        out.push(path.to_path_buf());
    }
    Ok(())
}

fn build_labels(book: &BookDb, csa_paths: &[PathBuf], min_rating: u32) -> Aggregator {
    csa_paths
        .par_iter()
        .map(|path| process_game(path, book, min_rating))
        .reduce(Aggregator::default, merge)
}

fn process_game(path: &Path, book: &BookDb, min_rating: u32) -> Aggregator {
    let mut agg = Aggregator::default();
    agg.stats.files_seen = 1;

    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            agg.stats.parse_errors += 1;
            return agg;
        }
    };
    let (mut pos, parsed, info) = match rshogi_csa::parse_csa_full(&text) {
        Ok(parsed) => parsed,
        Err(_) => {
            agg.stats.parse_errors += 1;
            return agg;
        }
    };

    if min_rating > 0 {
        if info.black_rating.is_none() || info.white_rating.is_none() {
            agg.stats.no_rating += 1;
            return agg;
        }
        if !info.both_ratings_at_least(min_rating as f64) {
            agg.stats.rating_skipped += 1;
            return agg;
        }
    }

    let (normal_moves, kachi_side) = split_normal_moves_and_kachi_side(pos.side_to_move, &parsed);
    agg.stats.eligible_games += 1;
    if kachi_side.is_some() {
        agg.stats.kachi_games += 1;
    }

    let mut hits = Vec::new();
    let mut replay_failed = false;
    for csa_move in normal_moves {
        let key = strip_ply(&pos.to_sfen()).to_string();
        let move_usi = match rshogi_csa::csa_move_to_usi(&csa_move.mv, &pos) {
            Ok(move_usi) => move_usi,
            Err(_) => {
                replay_failed = true;
                break;
            }
        };

        if let Some(matched) = match_book_move(book, &key, &move_usi) {
            let side_for_label = if matched.flipped {
                kachi_side.map(Side::other)
            } else {
                kachi_side
            };
            hits.push(BufferedHit {
                key: matched.key,
                move_usi: matched.move_usi,
                kachi_side: side_for_label,
                flipped: matched.flipped,
            });
        }

        if pos.apply_csa_move(&csa_move.mv).is_err() {
            replay_failed = true;
            break;
        }
    }

    if replay_failed {
        agg.stats.replay_errors += 1;
        return agg;
    }

    if !hits.is_empty() {
        agg.stats.games_with_hits += 1;
    }
    for hit in hits {
        if hit.flipped {
            agg.stats.flipped_hits += 1;
        } else {
            agg.stats.direct_hits += 1;
        }
        agg.record(hit.key, hit.move_usi, hit.kachi_side);
    }
    agg
}

fn split_normal_moves_and_kachi_side(
    initial_side: CsaColor,
    parsed: &[ParsedMove],
) -> (Vec<rshogi_csa::CsaMove>, Option<Side>) {
    let mut normal_moves = Vec::new();
    let mut side_to_move = Side::from(initial_side);

    for parsed_move in parsed {
        match parsed_move {
            ParsedMove::Normal(csa_move) => {
                normal_moves.push(csa_move.clone());
                side_to_move = side_to_move.other();
            }
            ParsedMove::Special(SpecialMove::Win) => return (normal_moves, Some(side_to_move)),
            ParsedMove::Special(_) => return (normal_moves, None),
        }
    }
    (normal_moves, None)
}

fn match_book_move(book: &BookDb, key: &str, move_usi: &str) -> Option<MatchedBookMove> {
    if let Some(moves) = book.nodes.get(key) {
        if moves.contains(move_usi) {
            return Some(MatchedBookMove {
                key: key.to_string(),
                move_usi: move_usi.to_string(),
                flipped: false,
            });
        }
        return None;
    }

    let flipped_key = flipped_lookup_key(key)?;
    let moves = book.nodes.get(&flipped_key)?;
    let flipped_move = rshogi_book::flip_usi_move(move_usi)?;
    if !moves.contains(&flipped_move) {
        return None;
    }
    Some(MatchedBookMove {
        key: flipped_key,
        move_usi: flipped_move,
        flipped: true,
    })
}

fn flipped_lookup_key(key: &str) -> Option<String> {
    let with_ply = if key.split_whitespace().count() == 3 {
        format!("{key} 1")
    } else {
        key.to_string()
    };
    rshogi_book::flipped_key(&with_ply).map(|flipped| strip_ply(&flipped).to_string())
}

fn merge(mut a: Aggregator, b: Aggregator) -> Aggregator {
    a.stats.add(&b.stats);
    for (key, value) in b.labels {
        a.labels.entry(key).or_default().merge_from(value);
    }
    a
}

fn write_sidecar(agg: &Aggregator, out: &Path) -> Result<()> {
    let mut content = String::new();
    for ((sfen_key, move_usi), label) in &agg.labels {
        let record = SidecarRecord {
            sfen_key,
            move_usi,
            games: label.games,
            kachi_black: label.kachi_black,
            kachi_white: label.kachi_white,
        };
        let line = serde_json::to_string(&record)
            .with_context(|| format!("sidecar JSON 書き出しに失敗: {}", out.display()))?;
        content.push_str(&line);
        content.push('\n');
    }
    write_atomic(out, &content).with_context(|| format!("sidecar を書けません: {}", out.display()))
}

fn write_report(agg: &Aggregator, book: &BookDb, path: &Path) -> Result<()> {
    let mut text = String::new();
    use std::fmt::Write as _;

    let covered_50 = agg.labels.values().filter(|label| label.games >= 50).count();
    let coverage = ratio(covered_50 as u64, book.move_count as u64);
    let mut rates: Vec<f64> = agg.labels.values().map(LabelAgg::kachi_rate).collect();
    rates.sort_by(f64::total_cmp);

    writeln!(text, "# book_kachi_label report")?;
    writeln!(text)?;
    writeln!(text, "## summary")?;
    writeln!(text)?;
    writeln!(text, "- CSA files: {}", agg.stats.files_seen)?;
    writeln!(text, "- eligible games: {}", agg.stats.eligible_games)?;
    writeln!(text, "- KACHI games: {}", agg.stats.kachi_games)?;
    writeln!(text, "- games with book hits: {}", agg.stats.games_with_hits)?;
    writeln!(text, "- parse errors: {}", agg.stats.parse_errors)?;
    writeln!(text, "- replay errors: {}", agg.stats.replay_errors)?;
    writeln!(text, "- no rating skipped: {}", agg.stats.no_rating)?;
    writeln!(text, "- low rating skipped: {}", agg.stats.rating_skipped)?;
    writeln!(text, "- direct hits: {}", agg.stats.direct_hits)?;
    writeln!(text, "- flipped hits: {}", agg.stats.flipped_hits)?;
    writeln!(text)?;
    writeln!(text, "## book coverage")?;
    writeln!(text)?;
    writeln!(text, "- book nodes: {}", book.nodes.len())?;
    writeln!(text, "- book node-moves: {}", book.move_count)?;
    writeln!(text, "- labeled node-moves: {}", agg.labels.len())?;
    writeln!(text, "- games>=50 coverage: {covered_50}/{} ({coverage:.2}%)", book.move_count)?;
    writeln!(text)?;
    write_rate_distribution(&mut text, &rates)?;
    write_top_rates(&mut text, agg)?;

    write_atomic(path, &text).with_context(|| format!("report を書けません: {}", path.display()))
}

fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        0.0
    } else {
        (num as f64 * 100.0) / den as f64
    }
}

fn write_rate_distribution(writer: &mut String, sorted_rates: &[f64]) -> Result<()> {
    use std::fmt::Write as _;

    writeln!(writer, "## KACHI rate distribution")?;
    writeln!(writer)?;
    if sorted_rates.is_empty() {
        writeln!(writer, "- no labeled node-moves")?;
        writeln!(writer)?;
        return Ok(());
    }

    writeln!(writer, "| metric | rate |")?;
    writeln!(writer, "|---|---:|")?;
    for pct in [50, 75, 90, 95, 99] {
        writeln!(writer, "| p{pct} | {:.4}% |", percentile_rate(sorted_rates, pct) * 100.0)?;
    }
    writeln!(writer, "| max | {:.4}% |", sorted_rates[sorted_rates.len() - 1] * 100.0)?;
    writeln!(writer)?;

    let buckets = rate_buckets(sorted_rates);
    writeln!(writer, "| bucket | node-moves |")?;
    writeln!(writer, "|---|---:|")?;
    for (name, count) in buckets {
        writeln!(writer, "| {name} | {count} |")?;
    }
    writeln!(writer)?;
    Ok(())
}

fn percentile_rate(sorted: &[f64], pct: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) * pct) / 100;
    sorted[idx]
}

fn rate_buckets(rates: &[f64]) -> Vec<(&'static str, usize)> {
    let mut buckets = vec![
        ("0%", 0usize),
        ("(0%, 1%]", 0),
        ("(1%, 2%]", 0),
        ("(2%, 5%]", 0),
        ("(5%, 10%]", 0),
        (">10%", 0),
    ];
    for &rate in rates {
        let idx = if rate == 0.0 {
            0
        } else if rate <= 0.01 {
            1
        } else if rate <= 0.02 {
            2
        } else if rate <= 0.05 {
            3
        } else if rate <= 0.10 {
            4
        } else {
            5
        };
        buckets[idx].1 += 1;
    }
    buckets
}

fn write_top_rates(writer: &mut String, agg: &Aggregator) -> Result<()> {
    use std::fmt::Write as _;

    let mut rows: Vec<(&String, &String, &LabelAgg)> =
        agg.labels.iter().map(|((key, mv), label)| (key, mv, label)).collect();
    rows.sort_by(|a, b| {
        b.2.kachi_rate()
            .total_cmp(&a.2.kachi_rate())
            .then_with(|| b.2.games.cmp(&a.2.games))
            .then_with(|| a.0.cmp(b.0))
            .then_with(|| a.1.cmp(b.1))
    });

    writeln!(writer, "## top KACHI rates")?;
    writeln!(writer)?;
    writeln!(writer, "| sfen_key | move | games | kachi | rate |")?;
    writeln!(writer, "|---|---:|---:|---:|---:|")?;
    for (key, move_usi, label) in rows.into_iter().take(20) {
        let kachi = label.kachi_black + label.kachi_white;
        writeln!(
            writer,
            "| `{}` | `{}` | {} | {} | {:.4}% |",
            key,
            move_usi,
            label.games,
            kachi,
            label.kachi_rate() * 100.0
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HIRATE_KEY: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
    const HIRATE_WHITE_KEY: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w -";

    fn book_text(lines: &[&str]) -> String {
        let mut text = "#YANEURAOU-DB2016 1.00\n".to_string();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    fn run_fixture(
        book_body: &str,
        games: &[(&str, &str)],
        min_rating: u32,
    ) -> (String, Aggregator) {
        let dir = tempfile::tempdir().unwrap();
        let book_path = dir.path().join("book.db");
        let corpus = dir.path().join("corpus");
        let out = dir.path().join("sidecar.jsonl");
        fs::create_dir(&corpus).unwrap();
        fs::write(&book_path, book_body).unwrap();
        for (name, body) in games {
            let path = corpus.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, body).unwrap();
        }

        let book = read_book_db(&book_path).unwrap();
        let paths = collect_csa_paths(&corpus).unwrap();
        let agg = build_labels(&book, &paths, min_rating);
        write_sidecar(&agg, &out).unwrap();
        (fs::read_to_string(out).unwrap(), agg)
    }

    #[test]
    fn deterministic_output_is_byte_identical() {
        let book = book_text(&[
            &format!("sfen {HIRATE_KEY} 1"),
            "7g7f none 0 0 1",
            "2g2f none 0 0 1",
        ]);
        let games = [
            ("b.csa", "V2.2\nPI\n+2726FU\n%TORYO\n"),
            ("a.csa", "V2.2\nPI\n+7776FU\n%TORYO\n"),
        ];

        let (first, _) = run_fixture(&book, &games, 0);
        let (second, _) = run_fixture(&book, &games, 0);
        assert_eq!(first, second);
        assert!(first.contains("\"move\":\"2g2f\""));
        assert!(first.contains("\"move\":\"7g7f\""));
    }

    #[test]
    fn kachi_side_is_side_to_move_at_special_move() {
        let book = book_text(&[&format!("sfen {HIRATE_KEY} 1"), "7g7f none 0 0 1"]);
        let games = [("kachi.csa", "V2.2\nPI\n+7776FU\n%KACHI\n")];

        let (jsonl, agg) = run_fixture(&book, &games, 0);
        assert_eq!(agg.stats.kachi_games, 1);
        assert!(jsonl.contains("\"games\":1"));
        assert!(jsonl.contains("\"kachi_black\":0"));
        assert!(jsonl.contains("\"kachi_white\":1"));
    }

    #[test]
    fn flipped_position_and_move_are_merged_into_book_key() {
        let book = book_text(&[&format!("sfen {HIRATE_KEY} 1"), "7g7f none 0 0 1"]);
        let games = [("white.csa", "V2.2\nPI\n-\n-3334FU\n%TORYO\n")];

        let (jsonl, agg) = run_fixture(&book, &games, 0);
        assert_eq!(agg.stats.direct_hits, 0);
        assert_eq!(agg.stats.flipped_hits, 1);
        assert!(jsonl.contains(&format!("\"sfen_key\":\"{HIRATE_KEY}\"")));
        assert!(jsonl.contains("\"move\":\"7g7f\""));
        assert!(jsonl.contains("\"games\":1"));
    }

    #[test]
    fn flipped_kachi_side_is_counted_in_book_coordinates() {
        let book = book_text(&[&format!("sfen {HIRATE_KEY} 1"), "7g7f none 0 0 1"]);
        let games = [("white.csa", "V2.2\nPI\n-\n-3334FU\n%KACHI\n")];

        let (jsonl, _) = run_fixture(&book, &games, 0);
        assert!(jsonl.contains("\"kachi_black\":0"), "flip 後の book 座標で数える:\n{jsonl}");
        assert!(jsonl.contains("\"kachi_white\":1"), "flip 後の book 座標で数える:\n{jsonl}");
    }

    #[test]
    fn rating_below_threshold_is_excluded() {
        let book = book_text(&[
            &format!("sfen {HIRATE_KEY} 1"),
            "7g7f none 0 0 1",
            "2g2f none 0 0 1",
        ]);
        let low =
            "V2.2\nN+A\nN-B\n'black_rate:A+x:3999.0\n'white_rate:B+y:4100.0\nPI\n+2726FU\n%TORYO\n";
        let high =
            "V2.2\nN+C\nN-D\n'black_rate:C+x:4200.0\n'white_rate:D+y:4100.0\nPI\n+7776FU\n%TORYO\n";
        let (jsonl, agg) = run_fixture(&book, &[("low.csa", low), ("high.csa", high)], 4000);

        assert_eq!(agg.stats.rating_skipped, 1);
        assert_eq!(agg.stats.eligible_games, 1);
        assert!(jsonl.contains("\"move\":\"7g7f\""));
        assert!(!jsonl.contains("\"move\":\"2g2f\""));
    }

    #[test]
    fn rateless_game_is_excluded_when_min_rating_is_enabled() {
        let book = book_text(&[&format!("sfen {HIRATE_KEY} 1"), "7g7f none 0 0 1"]);
        let (jsonl, agg) =
            run_fixture(&book, &[("no_rate.csa", "V2.2\nPI\n+7776FU\n%TORYO\n")], 4000);

        assert_eq!(agg.stats.no_rating, 1);
        assert_eq!(agg.stats.eligible_games, 0);
        assert!(jsonl.is_empty());
    }

    #[test]
    fn move_outside_book_node_is_not_counted() {
        let book = book_text(&[&format!("sfen {HIRATE_KEY} 1"), "7g7f none 0 0 1"]);
        let (jsonl, agg) = run_fixture(&book, &[("outside.csa", "V2.2\nPI\n+2726FU\n%TORYO\n")], 0);

        assert_eq!(agg.stats.direct_hits, 0);
        assert_eq!(agg.labels.len(), 0);
        assert!(jsonl.is_empty());
    }

    #[test]
    fn replay_failure_discards_all_buffered_hits() {
        let book = book_text(&[&format!("sfen {HIRATE_KEY} 1"), "7g7f none 0 0 1"]);
        // 1 手目は book hit するが、2 手目は手番違いで apply_csa_move が失敗する。
        // 対局全体の replay が壊れたので、1 手目の buffered hit も破棄される。
        let broken = "V2.2\nPI\n+7776FU\n+7776FU\n%TORYO\n";
        let (jsonl, agg) = run_fixture(&book, &[("broken.csa", broken)], 0);

        assert_eq!(agg.stats.replay_errors, 1);
        assert_eq!(agg.stats.direct_hits, 0);
        assert_eq!(agg.stats.games_with_hits, 0);
        assert!(agg.labels.is_empty());
        assert!(jsonl.is_empty());
    }

    #[test]
    fn position_outside_book_is_not_counted() {
        let after_7g7f = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w -";
        let book = book_text(&[&format!("sfen {after_7g7f} 2"), "3c3d none 0 0 1"]);
        let (jsonl, agg) = run_fixture(&book, &[("outside.csa", "V2.2\nPI\n+7776FU\n%TORYO\n")], 0);

        assert_eq!(agg.labels.len(), 0);
        assert!(jsonl.is_empty());
    }

    #[test]
    fn report_contains_coverage_and_top_rates() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report.md");
        let book = read_book_db_from_text(&book_text(&[
            &format!("sfen {HIRATE_KEY} 1"),
            "7g7f none 0 0 1",
        ]));
        let mut agg = Aggregator::default();
        agg.stats.eligible_games = 2;
        agg.stats.kachi_games = 1;
        agg.record(HIRATE_KEY.to_string(), "7g7f".to_string(), Some(Side::Black));
        agg.record(HIRATE_KEY.to_string(), "7g7f".to_string(), None);

        write_report(&agg, &book, &report).unwrap();
        let text = fs::read_to_string(report).unwrap();
        assert!(text.contains("eligible games: 2"));
        assert!(text.contains("KACHI games: 1"));
        assert!(text.contains("top KACHI rates"));
    }

    #[test]
    fn output_writers_create_missing_parent_dirs_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("nested/sidecar/out.jsonl");
        let report = dir.path().join("nested/report/report.md");
        let book = read_book_db_from_text(&book_text(&[
            &format!("sfen {HIRATE_KEY} 1"),
            "7g7f none 0 0 1",
        ]));
        let mut agg = Aggregator::default();
        agg.stats.eligible_games = 1;
        agg.record(HIRATE_KEY.to_string(), "7g7f".to_string(), Some(Side::Black));

        write_sidecar(&agg, &sidecar).unwrap();
        write_report(&agg, &book, &report).unwrap();

        assert!(fs::read_to_string(sidecar).unwrap().contains("\"move\":\"7g7f\""));
        assert!(fs::read_to_string(report).unwrap().contains("book_kachi_label report"));
    }

    fn read_book_db_from_text(text: &str) -> BookDb {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.db");
        fs::write(&path, text).unwrap();
        read_book_db(&path).unwrap()
    }

    #[test]
    fn split_kachi_side_handles_black_and_white() {
        let (_, side) = split_normal_moves_and_kachi_side(
            CsaColor::Black,
            &[ParsedMove::Special(SpecialMove::Win)],
        );
        assert_eq!(side, Some(Side::Black));

        let parsed = rshogi_csa::parse_csa_full("V2.2\nPI\n+7776FU\n%KACHI\n").unwrap().1;
        let (_, side) = split_normal_moves_and_kachi_side(CsaColor::Black, &parsed);
        assert_eq!(side, Some(Side::White));
    }

    #[test]
    fn flipped_lookup_key_strips_ply() {
        assert_eq!(flipped_lookup_key(HIRATE_WHITE_KEY).as_deref(), Some(HIRATE_KEY));
    }
}
