//! floodgate CSA 由来の入玉アンカー局面を gensfen 用 startpos に変換する。
//!
//! # 手動確認手順
//!
//! 1. 外部スクリプトで `csa_path<TAB>black_entry_ply<TAB>white_entry_ply<TAB>total_plies`
//!    形式の manifest を1行以上用意する。
//! 2. `cargo run -p tools --bin nyugyoku_gensfen -- --manifest /path/to/manifest.tsv --out-dir /tmp/nyugyoku-startpos`
//!    を実行する。
//! 3. `/tmp/nyugyoku-startpos/startpos.txt` を
//!    `cargo run -p tools --bin gensfen -- --startpos-file /tmp/nyugyoku-startpos/startpos.txt ...`
//!    に渡し、`provenance.tsv` の `startpos_line` と gensfen result の `start_pos_index`
//!    が対応することを確認する。

use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use rshogi_core::position::Position as CorePosition;
use rshogi_core::types::{EnteringKingRule, Move};
use rshogi_csa::{ParsedMove, parse_csa_full};

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

    /// startpos.txt と provenance.tsv の出力先
    #[arg(long)]
    out_dir: PathBuf,

    /// SFEN dedup テーブルのエントリ数（2 冪へ切り上げてから確保、メモリ = entries x 8B）。
    /// direct-mapped のため重複検出漏れは使用率に比例して増える。
    /// 想定ユニーク局面数の数倍を指定する。
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    dedup_hash_entries: u64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractedPosition {
    sfen: String,
    source_csa: PathBuf,
    anchor_ply: u32,
    anchor_kind: &'static str,
    entry_side: char,
    eval_cp: Option<i32>,
    total_plies: u32,
    source_year: Option<u16>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(&cli.manifest, &cli.out_dir, cli.dedup_hash_entries)
}

fn run(manifest: &Path, out_dir: &Path, dedup_hash_entries: u64) -> Result<()> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;
    let startpos_path = out_dir.join("startpos.txt");
    let provenance_path = out_dir.join("provenance.tsv");
    let mut startpos = BufWriter::new(
        File::create(&startpos_path)
            .with_context(|| format!("failed to create {}", startpos_path.display()))?,
    );
    let mut provenance = BufWriter::new(
        File::create(&provenance_path)
            .with_context(|| format!("failed to create {}", provenance_path.display()))?,
    );

    writeln!(
        provenance,
        "startpos_line\tsource_csa\tanchor_ply\tanchor_kind\tentry_side\teval_cp\ttotal_plies\tsource_year"
    )?;

    let mut seen = FingerprintDedup::new(dedup_hash_entries)?;
    let mut next_line = 1usize;
    let mut skipped_rows = 0usize;
    let file = File::open(manifest)
        .with_context(|| format!("failed to open manifest {}", manifest.display()))?;
    for (line_idx, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let row = parse_manifest_row(trimmed)
            .with_context(|| format!("invalid manifest line {}", line_idx + 1))?;
        // CSA 側の問題（読込失敗・パース失敗）は 1 行の異常で全体を落とさず skip する。
        // manifest 自体の形式異常は上の hard error のまま。
        let extracted = match extract_from_row(&row) {
            Ok(extracted) => extracted,
            Err(e) => {
                eprintln!("warning: skipping manifest line {}: {e:#}", line_idx + 1);
                skipped_rows += 1;
                continue;
            }
        };
        for item in extracted {
            if seen.check_and_insert(sfen_fingerprint(&item.sfen)) {
                continue;
            }
            writeln!(startpos, "position sfen {}", item.sfen)?;
            writeln!(
                provenance,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                next_line,
                item.source_csa.display(),
                item.anchor_ply,
                item.anchor_kind,
                item.entry_side,
                item.eval_cp.map_or_else(String::new, |v| v.to_string()),
                item.total_plies,
                item.source_year.map_or_else(String::new, |v| v.to_string()),
            )?;
            next_line += 1;
        }
    }

    if next_line == 1 {
        drop(startpos);
        drop(provenance);
        let _ = fs::remove_file(&startpos_path);
        let _ = fs::remove_file(&provenance_path);
        bail!("no start positions extracted from manifest {}", manifest.display());
    }

    startpos.flush()?;
    provenance.flush()?;
    eprintln!(
        "extracted {} start positions ({} manifest rows skipped)",
        next_line - 1,
        skipped_rows
    );
    Ok(())
}

/// dedup 用の SFEN 64bit 指紋（固定シード SipHash）。
///
/// 別局面が同一指紋になる確率は 10 億件でも実質ゼロ（期待衝突数 ~0.03 件）で、
/// 衝突しても開始局面が 1 件落ちるだけなので許容する。
fn sfen_fingerprint(sfen: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    sfen.hash(&mut hasher);
    hasher.finish()
}

/// gensfen の SharedDedupHash と同じ direct-mapped 固定サイズ dedup テーブル。
///
/// 数億局面規模の抽出で SFEN や指紋を全保持するとピークメモリが入力件数に比例するため、
/// entries x 8B の固定メモリに抑える。direct-mapped なのでスロット上書きによる
/// 重複検出漏れは使用率（挿入済みユニーク数 / entries）に比例して増える（使用率 1 で
/// 新規挿入の約 37% が既存スロットに衝突）。漏れは重複開始局面が残るだけで実害は軽い。
struct FingerprintDedup {
    table: Vec<u64>,
    mask: u64,
}

/// 8B/エントリで 1TB。これを超える指定は入力ミスとみなす
const MAX_DEDUP_ENTRIES: u64 = 1 << 37;

impl FingerprintDedup {
    fn new(entries: u64) -> Result<Self> {
        let size = entries
            .max(1)
            .checked_next_power_of_two()
            .filter(|&s| s <= MAX_DEDUP_ENTRIES)
            .ok_or_else(|| {
            anyhow!("--dedup-hash-entries too large: {entries} (max {MAX_DEDUP_ENTRIES})")
        })?;
        Ok(Self {
            table: vec![0; size as usize],
            mask: size - 1,
        })
    }

    /// 重複なら true を返し、新規なら挿入して false を返す
    fn check_and_insert(&mut self, key: u64) -> bool {
        // key=0 は未使用エントリと区別できないので特殊扱い
        let effective_key = if key == 0 { 1 } else { key };
        let idx = (effective_key & self.mask) as usize;
        if self.table[idx] == effective_key {
            return true;
        }
        self.table[idx] = effective_key;
        false
    }
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
    let max_anchor = total_plies as i32 - 8;
    for (offset, anchor_kind) in OFFSETS {
        let anchor = entry_ply + offset;
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
    let mut evals = parse_post_move_eval_comments(&text);
    let (initial_pos, parsed, _info) = parse_csa_full(&text)
        .with_context(|| format!("failed to parse {}", row.csa_path.display()))?;
    let normal_moves: Vec<_> = parsed
        .iter()
        .filter_map(|pm| match pm {
            ParsedMove::Normal(cm) => Some(cm.mv.as_str()),
            ParsedMove::Special(_) => None,
        })
        .collect();
    // 評価値コメントの対応付けは行スキャンで行うため、CSA パーサの指し手数と一致しない
    // 棋譜（1 行複数手など）では index がずれる。ずれたまま誤った eval を記録するより
    // eval なし扱いにする。
    if evals.len() != normal_moves.len() {
        eprintln!(
            "warning: {}: eval comment scan found {} move lines but parser found {} moves; dropping eval_cp",
            row.csa_path.display(),
            evals.len(),
            normal_moves.len()
        );
        evals.clear();
    }
    // manifest の total_plies は外部生成のため、CSA パーサの実手数と食い違いうる。
    // 「アンカー後に 8 手以上残る」の保証は実手数側でも課し、超過アンカーは除外する。
    if row.total_plies as usize != normal_moves.len() {
        eprintln!(
            "warning: {}: manifest total_plies {} != parsed move count {}; clipping anchors to parsed moves",
            row.csa_path.display(),
            row.total_plies,
            normal_moves.len()
        );
    }

    let mut out = Vec::new();
    for candidate in candidates {
        let anchor_idx = candidate.anchor_ply as usize;
        if anchor_idx + 8 > normal_moves.len() {
            continue;
        }

        let mut pos = initial_pos.clone();
        for mv in normal_moves.iter().take(anchor_idx) {
            pos.apply_csa_move(mv).with_context(|| {
                format!(
                    "{}: failed to replay move {} for anchor {}",
                    row.csa_path.display(),
                    mv,
                    candidate.anchor_ply
                )
            })?;
        }

        let sfen = pos.to_sfen();
        if is_declarable_for_side_to_move(&sfen)? {
            continue;
        }
        out.push(ExtractedPosition {
            sfen,
            source_csa: row.csa_path.clone(),
            anchor_ply: candidate.anchor_ply,
            anchor_kind: candidate.anchor_kind,
            entry_side: candidate.entry_side,
            eval_cp: evals.get(anchor_idx - 1).copied().flatten(),
            total_plies: row.total_plies,
            source_year: extract_source_year(&row.csa_path),
        });
    }
    Ok(out)
}

fn is_declarable_for_side_to_move(sfen: &str) -> Result<bool> {
    let mut pos = CorePosition::new();
    pos.set_sfen(sfen)
        .map_err(|e| anyhow!("invalid SFEN after CSA replay: {e:?}: {sfen}"))?;
    Ok(pos.declaration_win(EnteringKingRule::Point27) != Move::NONE)
}

fn parse_post_move_eval_comments(text: &str) -> Vec<Option<i32>> {
    fn leading_score(rest: &str) -> Option<i32> {
        rest.split_whitespace().next().and_then(|s| s.parse().ok())
    }

    let mut scores = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if is_csa_move_line(line) {
            scores.push(None);
        } else if let Some(rest) = line.strip_prefix("'**")
            && let (Some(last), Some(cp)) = (scores.last_mut(), leading_score(rest))
        {
            *last = Some(cp);
        }
    }
    scores
}

fn is_csa_move_line(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() >= 7 && (b[0] == b'+' || b[0] == b'-') && b[1..5].iter().all(u8::is_ascii_digit)
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

    #[test]
    fn anchor_candidates_clip_by_bounds() {
        let row = ManifestRow {
            csa_path: PathBuf::from("a.csa"),
            black_entry_ply: 20,
            white_entry_ply: -1,
            total_plies: 50,
        };
        let anchors = anchor_candidates(&row);
        assert_eq!(
            anchors,
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
    fn run_extracts_clips_filters_declarable_and_dedups() {
        let dir = tempfile::tempdir().expect("tempdir");
        let year_dir = dir.path().join("2024");
        fs::create_dir(&year_dir).expect("mkdir");

        let normal = year_dir.join("normal.csa");
        let mut normal_text = simple_board();
        normal_text.push('\n');
        normal_text.push_str(&cycle_moves(40, "+7565KI", "+6575KI", "-1939KY", "-3919KY"));
        normal_text.push_str("%TORYO\n");
        fs::write(&normal, normal_text).expect("write normal");

        let kachi = year_dir.join("kachi.csa");
        let mut kachi_text = declarable_board();
        kachi_text.push('\n');
        kachi_text.push_str(&cycle_moves(40, "+7161KI", "+6171KI", "-1939KY", "-3919KY"));
        kachi_text.push_str("%KACHI\n");
        fs::write(&kachi, kachi_text).expect("write kachi");

        let manifest = dir.path().join("manifest.tsv");
        fs::write(
            &manifest,
            format!("{}\t20\t20\t30\n{}\t20\t-1\t30\n", normal.display(), kachi.display()),
        )
        .expect("write manifest");

        let out_dir = dir.path().join("out");
        run(&manifest, &out_dir, 1 << 16).expect("run");

        let startpos = fs::read_to_string(out_dir.join("startpos.txt")).expect("startpos");
        let startpos_lines: Vec<_> = startpos.lines().collect();
        assert_eq!(startpos_lines.len(), 1);
        assert!(startpos_lines[0].starts_with("position sfen "));

        let provenance = fs::read_to_string(out_dir.join("provenance.tsv")).expect("provenance");
        let rows: Vec<_> = provenance.lines().collect();
        assert_eq!(rows.len(), 2);
        let cols: Vec<_> = rows[1].split('\t').collect();
        assert_eq!(cols[0], "1");
        assert_eq!(cols[2], "20");
        assert_eq!(cols[3], "entry");
        assert_eq!(cols[4], "b");
        assert_eq!(cols[5], "20");
        assert_eq!(cols[6], "30");
        assert_eq!(cols[7], "2024");
    }

    #[test]
    fn run_skips_unreadable_csa_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let normal = dir.path().join("normal.csa");
        let mut normal_text = simple_board();
        normal_text.push('\n');
        normal_text.push_str(&cycle_moves(40, "+7565KI", "+6575KI", "-1939KY", "-3919KY"));
        normal_text.push_str("%TORYO\n");
        fs::write(&normal, normal_text).expect("write normal");

        let manifest = dir.path().join("manifest.tsv");
        // 1 行目: 存在しない CSA (skip)、2 行目: total_plies 過大 (アンカーを実手数に clip、
        // 残る anchor 20 は 3 行目と同一局面で dedup)、3 行目: 正常
        fs::write(
            &manifest,
            format!(
                "{}\t20\t-1\t30\n{}\t60\t-1\t100\n{}\t20\t-1\t30\n",
                dir.path().join("missing.csa").display(),
                normal.display(),
                normal.display(),
            ),
        )
        .expect("write manifest");

        let out_dir = dir.path().join("out");
        run(&manifest, &out_dir, 1 << 16).expect("run must skip broken rows");

        let startpos = fs::read_to_string(out_dir.join("startpos.txt")).expect("startpos");
        assert_eq!(startpos.lines().count(), 1);
    }

    #[test]
    fn extract_drops_evals_when_scan_disagrees_with_parser() {
        let dir = tempfile::tempdir().expect("tempdir");
        let csa = dir.path().join("multi.csa");
        let mut text = simple_board();
        text.push('\n');
        text.push_str(&cycle_moves(40, "+7565KI", "+6575KI", "-1939KY", "-3919KY"));
        // パーサは `+`/`-` 始まりの 7 文字以上を手として数えるが、行スキャンは
        // 座標 digit を要求するため数えない → 手数不一致になる行
        text.push_str("+ABCDEFG\n%TORYO\n");
        fs::write(&csa, text).expect("write csa");

        let row = ManifestRow {
            csa_path: csa,
            black_entry_ply: 20,
            white_entry_ply: -1,
            total_plies: 42,
        };
        let extracted = extract_from_row(&row).expect("extract");
        assert!(!extracted.is_empty());
        assert!(extracted.iter().all(|item| item.eval_cp.is_none()));
    }

    #[test]
    fn run_bails_when_manifest_extracts_no_positions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = dir.path().join("manifest.tsv");
        fs::write(&manifest, "# comment only\n\n").expect("write manifest");

        let out_dir = dir.path().join("out");
        let err = run(&manifest, &out_dir, 1 << 16).expect_err("empty extraction must fail");
        assert!(err.to_string().contains("no start positions extracted"), "{err}");
        assert!(!out_dir.join("startpos.txt").exists());
        assert!(!out_dir.join("provenance.tsv").exists());
    }

    #[test]
    fn parses_post_move_eval_comments_only() {
        let text = "PI\n'* 999\n+7776FU\n'** 12 pv\n-3334FU\n'** -5\n";
        assert_eq!(parse_post_move_eval_comments(text), vec![Some(12), Some(-5)]);
    }
}
