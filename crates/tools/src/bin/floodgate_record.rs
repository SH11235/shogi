//! csa_client が per-game に吐く JSONL(`meta`/`move`/`result` 行)から、ある
//! エンジンの戦績を集計する CLI。floodgate 連続対局のように相手が毎局変わる
//! ログ群を、先後別・相手別・非勝一覧・実戦 NPS で要約する。
//!
//! JSONL は両対局者の指し手を `engine` 名付きで記録するため、手番・相手・勝敗は
//! ファイル名に依らず JSONL だけで判定できる(ply1 の `engine` が先手)。
//!
//! # 例
//! ```text
//! # ~/floodgate/records/jsonl を集計(対象エンジンは自動判定)
//! floodgate_record --dir ~/floodgate/records/jsonl
//!
//! # 対象エンジンを明示し、注目相手を指定
//! floodgate_record --dir ./jsonl --me RAMU_TF --watch Suisho,dlshogi,nshogi
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Aggregate an engine's win/loss record from csa_client per-game JSONL"
)]
struct Cli {
    /// 集計対象 JSONL(`*.jsonl`)のあるディレクトリ(再帰なし)
    #[arg(long, default_value = ".")]
    dir: PathBuf,

    /// 集計対象エンジン名。省略時は全 JSONL に最も多く出現するエンジンを自動判定。
    #[arg(long)]
    me: Option<String>,

    /// 注目相手(カンマ区切り・部分一致)。指定時のみ「注目相手との対戦」節を出力。
    #[arg(long, value_delimiter = ',')]
    watch: Vec<String>,
}

/// JSONL 1 行の必要フィールドだけを拾う(未使用 type 行は無視)。
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: String,
    ply: Option<u32>,
    engine: Option<String>,
    eval: Option<Eval>,
    winner: Option<String>,
    reason: Option<String>,
    plies: Option<u32>,
}

#[derive(Deserialize)]
struct Eval {
    nps: Option<f64>,
    time_ms: Option<u64>,
}

/// 1 局分の集計素材。
struct Game {
    /// ファイル名(datetime prefix を含む。表示・整列用)
    stem: String,
    sente: String,
    gote: String,
    /// 勝者名。引き分けは `None`。
    winner: Option<String>,
    reason: String,
    plies: u32,
    /// engine 名 -> その engine の nps サンプル(time_ms>=500 の本探索のみ)
    nps: BTreeMap<String, Vec<f64>>,
}

/// me から見た 1 局の結果。
#[derive(Clone, Copy, PartialEq)]
enum Res {
    Win,
    Loss,
    Draw,
}

fn parse_game(path: &std::path::Path) -> Result<Option<Game>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let mut sente: Option<String> = None;
    let mut engines: Vec<String> = Vec::new(); // 出現順の distinct engine
    let mut nps: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut winner: Option<String> = None;
    let mut reason = String::new();
    let mut plies = 0u32;
    let mut has_result = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(l) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        match l.kind.as_str() {
            "move" => {
                if let Some(eng) = l.engine {
                    if l.ply == Some(1) {
                        sente = Some(eng.clone());
                    }
                    if !engines.contains(&eng) {
                        engines.push(eng.clone());
                    }
                    if let Some(ev) = l.eval
                        && let Some(n) = ev.nps
                        && ev.time_ms.unwrap_or(0) >= 500
                    {
                        nps.entry(eng).or_default().push(n);
                    }
                }
            }
            "result" => {
                has_result = true;
                winner = l.winner.filter(|w| !w.is_empty());
                reason = l.reason.unwrap_or_default();
                plies = l.plies.unwrap_or(0);
            }
            _ => {}
        }
    }

    if !has_result {
        return Ok(None); // 進行中 / 未完了の局はスキップ
    }
    let sente = match sente {
        Some(s) => s,
        None => return Ok(None), // 手が 1 手も無い異常局
    };
    let gote = engines.iter().find(|e| **e != sente).cloned().unwrap_or_default();

    Ok(Some(Game {
        stem,
        sente,
        gote,
        winner,
        reason,
        plies,
        nps,
    }))
}

/// me から見た結果。`None` は集計対象外(中断/検閲/error など未完了局)。
///
/// winner なしの局は reason で判別する: `sennichite` / `max_moves` / `jishogi` は正当な
/// 引き分け、それ以外(csa_client が中断・検閲を `outcome="draw"` + `reason="interrupted"`
/// で書く等)は勝率を歪めないよう除外する。
fn result_for(g: &Game, me: &str) -> Option<Res> {
    match &g.winner {
        Some(w) if w == me => Some(Res::Win),
        Some(_) => Some(Res::Loss),
        None => match g.reason.as_str() {
            "sennichite" | "max_moves" | "jishogi" => Some(Res::Draw),
            _ => None,
        },
    }
}

fn median(xs: &mut [f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let pattern = cli.dir.join("*.jsonl");
    let pattern = pattern.to_string_lossy();
    let mut games: Vec<Game> = Vec::new();
    for entry in glob::glob(&pattern).with_context(|| format!("glob {pattern}"))? {
        let path = entry?;
        if let Some(g) = parse_game(&path)? {
            games.push(g);
        }
    }
    if games.is_empty() {
        bail!("完了局の JSONL が {} に見つからない", cli.dir.display());
    }
    games.sort_by(|a, b| a.stem.cmp(&b.stem));

    // 対象エンジン: 明示 or 「最も多くの局に出現するエンジン」を自動判定。
    let me = match cli.me {
        Some(m) => m,
        None => {
            let mut count: BTreeMap<&str, usize> = BTreeMap::new();
            for g in &games {
                *count.entry(g.sente.as_str()).or_default() += 1;
                if !g.gote.is_empty() {
                    *count.entry(g.gote.as_str()).or_default() += 1;
                }
            }
            count
                .into_iter()
                .max_by_key(|(_, c)| *c)
                .map(|(name, _)| name.to_string())
                .context("対象エンジンを自動判定できない")?
        }
    };

    // 集計
    let mut w = 0usize;
    let mut l = 0usize;
    let mut d = 0usize;
    // 先後別 [先, 後] それぞれ (W, L, D)
    let mut by_color = [[0usize; 3]; 2];
    // 相手別 (W, L, D)
    let mut by_opp: BTreeMap<String, [usize; 3]> = BTreeMap::new();
    let mut my_nps: Vec<f64> = Vec::new();
    let mut skipped_foreign = 0usize; // me が参加していない局
    let mut skipped_incomplete = 0usize; // 中断/検閲/error など未完了局

    for g in &games {
        if g.sente != me && g.gote != me {
            skipped_foreign += 1;
            continue;
        }
        let Some(res) = result_for(g, &me) else {
            skipped_incomplete += 1;
            continue;
        };
        let is_sente = g.sente == me;
        let opp = if is_sente { &g.gote } else { &g.sente };
        let idx = match res {
            Res::Win => 0,
            Res::Loss => 1,
            Res::Draw => 2,
        };
        match res {
            Res::Win => w += 1,
            Res::Loss => l += 1,
            Res::Draw => d += 1,
        }
        by_color[if is_sente { 0 } else { 1 }][idx] += 1;
        by_opp.entry(opp.clone()).or_default()[idx] += 1;
        if let Some(samples) = g.nps.get(&me) {
            my_nps.extend_from_slice(samples);
        }
    }

    let total = games.len();
    let n = w + l + d;
    let pct = |num: usize, den: usize| {
        if den == 0 {
            0.0
        } else {
            num as f64 / den as f64 * 100.0
        }
    };
    println!("集計対象: {me}   JSONL {total} 局 → 集計 {n} 局");
    if skipped_foreign > 0 || skipped_incomplete > 0 {
        println!(
            "  (除外: 対象不参加 {skipped_foreign} 局 / 中断・未完了 {skipped_incomplete} 局)"
        );
    }
    println!(
        "=== 通算: {w}勝 {l}敗 {d}分  ({n}局)  勝率 {:.0}% (引分除 {:.0}%) ===",
        pct(w, n),
        pct(w, w + l)
    );

    let names = ["先手番", "後手番"];
    for (i, name) in names.iter().enumerate() {
        let [cw, cl, cd] = by_color[i];
        let games_c = cw + cl + cd;
        println!(
            "  {name}: {cw}勝 {cl}敗 {cd}分 ({games_c}局)  勝率 {:.0}% (引分除 {:.0}%)",
            pct(cw, games_c),
            pct(cw, cw + cl)
        );
    }

    if !my_nps.is_empty() {
        let n_nps = my_nps.len();
        let med = median(&mut my_nps) / 1e6;
        println!("  実戦NPS(time_ms>=500 の median): {med:.2}M  (n={n_nps})");
    }

    println!("\n=== 相手別 ===");
    for (opp, [ow, ol, od]) in &by_opp {
        println!("  {ow}勝 {ol}敗 {od}分  vs {opp}");
    }

    // 後手勝ちは最上位帯では希少で価値が高いので、相手名付きで individually 列挙する。
    println!("\n=== 後手勝ち(価値大) ===");
    let mut any_gw = false;
    for g in &games {
        if g.gote != me {
            continue; // 後手が me の局のみ
        }
        if result_for(g, &me) != Some(Res::Win) {
            continue;
        }
        any_gw = true;
        let opp = &g.sente;
        let star = if !cli.watch.is_empty() && cli.watch.iter().any(|w| opp.contains(w.as_str())) {
            "  ★上位AI"
        } else {
            ""
        };
        println!("  {}  vs {opp}  ({}, {}手){star}", g.stem, g.reason, g.plies);
    }
    if !any_gw {
        println!("  (なし)");
    }

    for (title, target) in [("負け", Res::Loss), ("引分", Res::Draw)] {
        println!("\n=== {title} ===");
        let mut any = false;
        for g in &games {
            if g.sente != me && g.gote != me {
                continue;
            }
            if result_for(g, &me) != Some(target) {
                continue;
            }
            any = true;
            let is_sente = g.sente == me;
            let opp = if is_sente { &g.gote } else { &g.sente };
            let col = if is_sente { "先" } else { "後" };
            println!("  {}  {col}手  vs {opp}  ({}, {}手)", g.stem, g.reason, g.plies);
        }
        if !any {
            println!("  (なし)");
        }
    }

    if !cli.watch.is_empty() {
        println!("\n=== 注目相手との対戦 ===");
        for g in &games {
            if g.sente != me && g.gote != me {
                continue;
            }
            let is_sente = g.sente == me;
            let opp = if is_sente { &g.gote } else { &g.sente };
            if !cli.watch.iter().any(|w| opp.contains(w.as_str())) {
                continue;
            }
            let Some(res) = result_for(g, &me) else {
                continue;
            };
            let col = if is_sente { "先" } else { "後" };
            let mark = match res {
                Res::Win => "○",
                Res::Loss => "●",
                Res::Draw => "△",
            };
            println!("  {}  {col}手  {mark}  vs {opp}  ({})", g.stem, g.reason);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(sente: &str, gote: &str, winner: Option<&str>, reason: &str) -> Game {
        Game {
            stem: "t".to_owned(),
            sente: sente.to_owned(),
            gote: gote.to_owned(),
            winner: winner.map(str::to_owned),
            reason: reason.to_owned(),
            plies: 100,
            nps: BTreeMap::new(),
        }
    }

    #[test]
    fn result_classification() {
        let me = "ME";
        assert!(matches!(result_for(&game("ME", "X", Some("ME"), "win"), me), Some(Res::Win)));
        assert!(matches!(result_for(&game("X", "ME", Some("X"), "resign"), me), Some(Res::Loss)));
        // winner なし + 正当な引き分け reason → Draw
        assert!(matches!(result_for(&game("X", "ME", None, "sennichite"), me), Some(Res::Draw)));
        assert!(matches!(result_for(&game("X", "ME", None, "max_moves"), me), Some(Res::Draw)));
        // winner なし + 中断系 reason → 集計対象外(None)
        assert!(result_for(&game("X", "ME", None, "interrupted"), me).is_none());
    }

    #[test]
    fn median_odd_even() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&mut []), 0.0);
    }

    #[test]
    fn parse_game_reads_players_and_result() {
        // 先手=OPP, 後手=ME, ME(白)勝ちの最小 JSONL を temp file に書いて解析。
        let dir = std::env::temp_dir();
        let path = dir.join("fg_record_test_g1.jsonl");
        let body = concat!(
            r#"{"type":"meta","timestamp":"t"}"#,
            "\n",
            r#"{"type":"move","game_id":1,"ply":1,"engine":"OPP","eval":{"nps":1000000,"time_ms":600}}"#,
            "\n",
            r#"{"type":"move","game_id":1,"ply":2,"engine":"ME","eval":{"nps":2000000,"time_ms":600}}"#,
            "\n",
            r#"{"type":"result","game_id":1,"outcome":"white_win","reason":"win","plies":2,"winner":"ME"}"#,
            "\n",
        );
        std::fs::write(&path, body).unwrap();
        let g = parse_game(&path).unwrap().expect("完了局");
        assert_eq!(g.sente, "OPP");
        assert_eq!(g.gote, "ME");
        assert_eq!(g.winner.as_deref(), Some("ME"));
        assert_eq!(g.plies, 2);
        // time_ms>=500 の nps が両者ぶん取れている
        assert_eq!(g.nps.get("ME").map(Vec::len), Some(1));
        let _ = std::fs::remove_file(&path);
    }
}
