//! PSV / tournament JSONL / CSA 共通の棋譜プレイヤー TUI。
//!
//! 詳細は `crates/tools/docs/kifu_player.md` を参照。

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{ArgGroup, Parser};
use tools::replay::{CsaSource, GameSource, JsonlSource, PsvSource, tui};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "PSV / tournament JSONL / CSA の対局を TUI で再生する"
)]
#[command(group(ArgGroup::new("source").required(true).args(["psv", "tournament_dir", "csa"])))]
struct Cli {
    /// PSV (PackedSfenValue) ファイルを開く。連続した自己対局ストリームを想定する
    /// （shuffle_psv/merge_psv 等でシャッフル済みのプールは対局境界検出が機能しない）。
    #[arg(long)]
    psv: Option<PathBuf>,

    /// tournament の out-dir を開く（配下の `*-vs-*.jsonl` を横断して索引する）。
    #[arg(long)]
    tournament_dir: Option<PathBuf>,

    /// CSA 棋譜（rshogi csa_client 出力形式）を開く。ディレクトリ（配下の `*.csa` を
    /// 横断）または単一 `.csa` ファイルを指定する。
    #[arg(long)]
    csa: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // 排他・必須は clap の ArgGroup が保証する（同時指定・未指定はパース時に弾かれる）。
    let source: Box<dyn GameSource> = if let Some(path) = cli.psv {
        Box::new(PsvSource::new(path))
    } else if let Some(dir) = cli.tournament_dir {
        Box::new(JsonlSource::new(dir))
    } else if let Some(path) = cli.csa {
        Box::new(CsaSource::new(path))
    } else {
        bail!("--psv / --tournament-dir / --csa のいずれか一つを指定してください");
    };
    tui::run(source)
}
