# wdoor floodgate の対局を 1 コマンドでライブ観戦する (Windows PowerShell 版)。
# 裏で floodgate_pipeline live-mirror を起動し、前面で kifu_player TUI を開く。
# TUI を閉じる (q) とミラーも自動停止する。
#   .\watch.ps1                 → 当日の全対局
#   .\watch.ps1 名前[,名前...]  → 指定 AI の対局のみ (対局者名の部分一致)
# ミラー dir は既定 ~\floodgate-mirror\<対象>\ に残る (後から見返せる)。
param([string]$Watch = "")

$ErrorActionPreference = "Stop"
$repo = if ($env:RSHOGI_REPO) { $env:RSHOGI_REPO } else { Join-Path $HOME "development/rshogi" }
$pipeline = if ($env:FLOODGATE_PIPELINE) { $env:FLOODGATE_PIPELINE } else { Join-Path $repo "target/release/floodgate_pipeline.exe" }
$player = if ($env:KIFU_PLAYER) { $env:KIFU_PLAYER } else { Join-Path $repo "target/release/kifu_player.exe" }
$base = if ($env:FLOODGATE_WATCH_DIR) { $env:FLOODGATE_WATCH_DIR } else { Join-Path $HOME "floodgate-mirror" }

if ($Watch) {
    $dir = Join-Path $base ($Watch -replace ",", "+")
    $mirrorArgs = @("live-mirror", "--out-dir", $dir, "--watch", $Watch)
} else {
    $dir = Join-Path $base "all"
    $mirrorArgs = @("live-mirror", "--out-dir", $dir)
}
New-Item -ItemType Directory -Force -Path $dir | Out-Null

# レート表キャッシュがあれば併記 (floodgate 運用機で stats.sh を回していれば存在する)
$playerArgs = @("--csa", $dir, "--live", "5")
$ratings = if ($env:FLOODGATE_RATINGS) { $env:FLOODGATE_RATINGS } else { Join-Path $HOME "floodgate/records/ratings_cache.tsv" }
if (Test-Path $ratings) { $playerArgs += @("--ratings", $ratings) }

# ミラーは裏で回す。ログは dir 内 (kifu_player は *.csa しか読まないので混ざらない)
$log = Join-Path $dir "live-mirror.log"
$mirror = Start-Process -FilePath $pipeline -ArgumentList $mirrorArgs `
    -RedirectStandardOutput $log -RedirectStandardError "$log.err" -NoNewWindow -PassThru
try {
    & $player @playerArgs
} finally {
    if (-not $mirror.HasExited) { Stop-Process -Id $mirror.Id -Force }
}
