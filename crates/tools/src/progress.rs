//! 複数ファイル / 長時間ジョブ向けの進捗表示ユーティリティ。
//!
//! rescore 系ツール（`rescore_psv` 等）と `book_rescore` で共有する。TTY では
//! `indicatif` のバー描画、非TTY では CR スパムを避けて定期的な 1 行ログに切替える。
//! overall バー（全ファイル分母）と per-file バーを束ね、% / pos/s / 残り時間 /
//! 完了予定時刻を表示する。

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};

// ============================================================
// 進捗表示（% / 残り時間 / 完了予定時刻）
// ============================================================

/// 件数を k/M 短縮表記にする（523456 -> "523.5k", 1000000 -> "1.00M"）。
fn compact_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1.0e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1.0e3)
    } else {
        n.to_string()
    }
}

/// 秒速を短縮表記にする（8338.0 -> "8.3k"）。
fn compact_rate(per_sec: f64) -> String {
    if per_sec >= 1_000_000.0 {
        format!("{:.2}M", per_sec / 1.0e6)
    } else if per_sec >= 1_000.0 {
        format!("{:.1}k", per_sec / 1.0e3)
    } else {
        format!("{per_sec:.0}")
    }
}

/// Duration を `H:MM:SS`（1h 以上）または `MM:SS` に整形する。
fn fmt_hms(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// `now + eta` をローカル時刻 `MM/DD HH:MM` にする。長時間ジョブは日跨ぎするため
/// 日付を必ず付ける。速度未確定（per_sec が 0）のときのみ `--/-- --:--`。
/// 完了時は eta=0 だが per_sec は確定済みで、`now + 0` が現在時刻＝到着時刻になる。
fn finish_clock(eta: Duration, per_sec: f64) -> String {
    if per_sec <= 0.0 {
        return "--/-- --:--".to_string();
    }
    match chrono::Duration::from_std(eta) {
        Ok(d) => (chrono::Local::now() + d).format("%m/%d %H:%M").to_string(),
        Err(_) => "--/-- --:--".to_string(),
    }
}

/// テンプレートに共通のカスタムキー（短縮件数・完了予定時刻）を足す。
fn with_progress_keys(style: ProgressStyle) -> ProgressStyle {
    style
        .with_key("cpos", |s: &ProgressState, w: &mut dyn std::fmt::Write| {
            let _ = write!(w, "{}", compact_count(s.pos()));
        })
        .with_key("clen", |s: &ProgressState, w: &mut dyn std::fmt::Write| {
            let _ = write!(w, "{}", compact_count(s.len().unwrap_or(0)));
        })
        .with_key("finish_at", |s: &ProgressState, w: &mut dyn std::fmt::Write| {
            let _ = write!(w, "{}", finish_clock(s.eta(), s.per_sec()));
        })
}

/// 非TTY 時に定期ログ行を間引くための状態。
struct LogThrottle {
    last: Option<Instant>,
    last_pct: f64,
}

const LOG_INTERVAL: Duration = Duration::from_secs(15);
const LOG_PCT_STEP: f64 = 5.0;

/// 複数入力ファイル（または 1 個の長時間ジョブ）の進捗管理。全ファイルを分母とした
/// overall バーと、ファイル単位バーを束ねる。TTY ではバー描画、非TTY では定期ログ行に
/// 切替える。
pub struct MultiFileProgress {
    multi: MultiProgress,
    /// overall バーは複数ファイル時のみ（単一ファイルは per-file バーがそのまま全体）。
    overall: Option<ProgressBar>,
    total_files: usize,
    is_tty: bool,
    /// 非TTY ログ行の先頭タグ（例: "rescore" → `[rescore] ...`）。ツール識別用。
    tag: &'static str,
    log: Arc<Mutex<LogThrottle>>,
}

impl MultiFileProgress {
    /// `tag` は非TTY ログ行の先頭に `[tag]` として付く（ツール名等）。
    pub fn new(overall_total: u64, total_files: usize, tag: &'static str) -> Self {
        let is_tty = std::io::stderr().is_terminal();
        let multi = if is_tty {
            MultiProgress::new()
        } else {
            // 非TTY ではバー描画をやめ、進捗は定期ログ行で出す（CR スパム回避）。
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };
        let overall = if total_files > 1 {
            let pb = multi.add(ProgressBar::new(overall_total));
            pb.set_style(
                with_progress_keys(ProgressStyle::default_bar().template(
                    "全体 {percent:>3}% {cpos}/{clen} ({prefix}) {per_sec} 残り {eta_precise} 完了 {finish_at}",
                ).expect("valid template")),
            );
            if is_tty {
                pb.enable_steady_tick(Duration::from_millis(500));
            }
            Some(pb)
        } else {
            None
        };
        Self {
            multi,
            overall,
            total_files,
            is_tty,
            tag,
            log: Arc::new(Mutex::new(LogThrottle {
                last: None,
                last_pct: 0.0,
            })),
        }
    }

    /// 完了済み / skip されたファイル 1 個分を overall に反映する（per-file バーは作らない）。
    pub fn skip_file(&self, n: u64) {
        if let Some(o) = &self.overall {
            o.inc(n);
        }
    }

    /// 処理するファイル 1 個分の per-file 進捗ハンドルを作る。`shard_idx` は 1 始まり。
    pub fn start_file(&self, label: &str, shard_idx: usize, len: u64) -> FileProgress {
        let file = self.multi.add(ProgressBar::new(len));
        if self.total_files > 1 {
            file.set_style(
                with_progress_keys(
                    ProgressStyle::default_bar()
                        .template("└ {prefix} {percent:>3}% {bar:30.cyan/blue} {cpos}/{clen}")
                        .expect("valid template"),
                )
                .progress_chars("██░"),
            );
            file.set_prefix(label.to_string());
            if let Some(o) = &self.overall {
                o.set_prefix(format!("shard {shard_idx}/{}", self.total_files));
            }
        } else {
            file.set_style(
                with_progress_keys(
                    ProgressStyle::default_bar()
                        .template("[{elapsed_precise}] {bar:30.cyan/blue} {percent:>3}% {cpos}/{clen} {per_sec} 残り {eta_precise} 完了 {finish_at}")
                        .expect("valid template"),
                )
                .progress_chars("██░"),
            );
        }
        if self.is_tty {
            file.enable_steady_tick(Duration::from_millis(500));
        }
        FileProgress {
            file,
            overall: self.overall.clone(),
            is_tty: self.is_tty,
            log: Arc::clone(&self.log),
            label: label.to_string(),
            shard_idx,
            total_files: self.total_files,
            tag: self.tag,
        }
    }

    /// 全ファイル完了。非TTY では overall の最終行を必ず 1 行出す。
    pub fn finish(&self) {
        if let Some(o) = &self.overall {
            if !self.is_tty {
                eprintln!(
                    "[{}] overall done {}/{} ({} files) {} pos/s took {}",
                    self.tag,
                    compact_count(o.position()),
                    compact_count(o.length().unwrap_or(0)),
                    self.total_files,
                    compact_rate(o.per_sec()),
                    fmt_hms(o.elapsed()),
                );
            }
            o.finish_and_clear();
        }
    }
}

/// 1 ファイル分の進捗ハンドル。`inc` で per-file と overall を同時に進める。
/// 複数スレッドから呼ぶ search/engine 用に `Clone`（内部の ProgressBar / Arc は共有）。
#[derive(Clone)]
pub struct FileProgress {
    file: ProgressBar,
    overall: Option<ProgressBar>,
    is_tty: bool,
    log: Arc<Mutex<LogThrottle>>,
    label: String,
    shard_idx: usize,
    total_files: usize,
    tag: &'static str,
}

impl FileProgress {
    /// resume で既処理だった分を起点として進める（per-file/overall とも前進）。
    /// 追記再開型ツールで、既に journal/出力に記録済みの件数を進捗の起点へ反映する
    /// 用途。出力を File::create で truncate して全件を再処理する型のツールでは呼ぶ
    /// 必要はない（全件 inc で 100% に到達するため）。
    pub fn advance_start(&self, n: u64) {
        if n == 0 {
            return;
        }
        self.file.inc(n);
        if let Some(o) = &self.overall {
            o.inc(n);
        }
    }

    /// per-file と overall を同時に `n` 前進させる。非TTY では throttle 付きの
    /// 定期ログ行を出す（TTY ではバー更新のみ）。
    pub fn inc(&self, n: u64) {
        self.file.inc(n);
        if let Some(o) = &self.overall {
            o.inc(n);
        }
        if !self.is_tty {
            self.maybe_log(false);
        }
    }

    /// per-file バーに一時メッセージを設定する（TTY 表示用）。
    pub fn set_message(&self, msg: &'static str) {
        self.file.set_message(msg);
    }

    /// per-file を正常完了として締める。複数ファイル時は overall を残して per-file
    /// バーだけ消し、単一ファイル時は完了メッセージ付きで確定する。非TTY では
    /// 最終ログ行を必ず 1 行出す。
    pub fn finish_with_message(&self, msg: &'static str) {
        if !self.is_tty {
            self.maybe_log(true);
        }
        if self.total_files > 1 {
            // 複数ファイル時は overall を残して per-file バーだけ消す。
            self.file.finish_and_clear();
        } else {
            self.file.finish_with_message(msg);
        }
    }

    /// per-file を中断（失敗）として締める。バーは消さずメッセージを残す。
    /// エラー経路での終了表示に使う。
    pub fn abandon_with_message(&self, msg: &'static str) {
        self.file.abandon_with_message(msg);
    }

    /// 非TTY 用の 1 行ログ。`force` 時は throttle を無視して必ず出す。
    fn maybe_log(&self, force: bool) {
        // overall（複数ファイル）か per-file（単一）を「全体進捗」として使う。
        let primary = self.overall.as_ref().unwrap_or(&self.file);
        let pos = primary.position();
        let len = primary.length().unwrap_or(0);
        let pct = if len > 0 {
            pos as f64 / len as f64 * 100.0
        } else {
            0.0
        };

        {
            let mut th = self.log.lock().expect("log throttle poisoned");
            if !force {
                let due_time = th.last.map(|t| t.elapsed() >= LOG_INTERVAL).unwrap_or(true);
                let due_pct = pct - th.last_pct >= LOG_PCT_STEP;
                if !due_time && !due_pct {
                    return;
                }
            }
            th.last = Some(Instant::now());
            th.last_pct = pct;
        }

        let rate = compact_rate(primary.per_sec());
        let elapsed = fmt_hms(primary.elapsed());
        // remaining = 残り時間（duration）、eta_clock = 完了予定の絶対時刻（ETA 本来の意味）。
        let remaining = fmt_hms(primary.eta());
        let eta_clock = finish_clock(primary.eta(), primary.per_sec());
        if self.overall.is_some() {
            let fpos = self.file.position();
            let flen = self.file.length().unwrap_or(0);
            let fpct = if flen > 0 {
                fpos as f64 / flen as f64 * 100.0
            } else {
                0.0
            };
            eprintln!(
                "[{}] overall {pct:.1}% {}/{} shard {}/{} ({} {fpct:.1}%) {rate} pos/s elapsed {elapsed} remaining {remaining} ETA {eta_clock}",
                self.tag,
                compact_count(pos),
                compact_count(len),
                self.shard_idx,
                self.total_files,
                self.label,
            );
        } else {
            eprintln!(
                "[{}] {} {pct:.1}% {}/{} {rate} pos/s elapsed {elapsed} remaining {remaining} ETA {eta_clock}",
                self.tag,
                self.label,
                compact_count(pos),
                compact_count(len),
            );
        }
    }
}

#[cfg(test)]
mod progress_format_tests {
    use super::{Duration, compact_count, compact_rate, fmt_hms};

    #[test]
    fn compact_count_thresholds() {
        assert_eq!(compact_count(0), "0");
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1_000), "1.0k");
        assert_eq!(compact_count(523_456), "523.5k");
        assert_eq!(compact_count(1_000_000), "1.00M");
        assert_eq!(compact_count(3_840_000), "3.84M");
    }

    #[test]
    fn compact_rate_thresholds() {
        assert_eq!(compact_rate(500.0), "500");
        assert_eq!(compact_rate(8338.0), "8.3k");
        assert_eq!(compact_rate(1_500_000.0), "1.50M");
    }

    #[test]
    fn fmt_hms_minutes_and_hours() {
        assert_eq!(fmt_hms(Duration::from_secs(0)), "00:00");
        assert_eq!(fmt_hms(Duration::from_secs(90)), "01:30");
        assert_eq!(fmt_hms(Duration::from_secs(600)), "10:00");
        assert_eq!(fmt_hms(Duration::from_secs(3661)), "1:01:01");
    }
}
