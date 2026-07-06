# wdoor floodgate の対局を 1 コマンドでライブ観戦する (Windows 版。要 PowerShell 7 = pwsh)。
# 裏で floodgate_pipeline live-mirror を起動し、前面で kifu_player TUI を開く。
# TUI を閉じる (q) とミラーも自動停止する。
#   .\watch.ps1                 → 当日の全対局
#   .\watch.ps1 名前[,名前...]  → 指定 AI の対局のみ (対局者名の部分一致)
# ミラー dir は既定 ~\floodgate-mirror\<対象>\ に残る (後から見返せる)。
# 複数指定は `名前,名前` のままでよい (PowerShell がカンマ区切りを配列として渡す)。
param([string[]]$Watch = @())

$ErrorActionPreference = "Stop"
$repo = if ($env:RSHOGI_REPO) { $env:RSHOGI_REPO } else { Join-Path $HOME "development/rshogi" }
$pipeline = if ($env:FLOODGATE_PIPELINE) { $env:FLOODGATE_PIPELINE } else { Join-Path $repo "target/release/floodgate_pipeline.exe" }
$player = if ($env:KIFU_PLAYER) { $env:KIFU_PLAYER } else { Join-Path $repo "target/release/kifu_player.exe" }
$base = if ($env:FLOODGATE_WATCH_DIR) { $env:FLOODGATE_WATCH_DIR } else { Join-Path $HOME "floodgate-mirror" }

# ミラーは裏で起動するためバイナリ不在のエラーがログ行きになり原因が見えない。先に検証する。
foreach ($bin in @($pipeline, $player)) {
    if (-not (Test-Path $bin)) {
        Write-Error ("{0} がありません。先にビルドしてください:`n" -f $bin +
            "  cargo build --release -p tools --bin kifu_player --bin floodgate_pipeline`n" +
            "  (repo: $repo。場所が違う場合は RSHOGI_REPO で指定)")
        exit 1
    }
}

$watchJoined = $Watch -join ","
if ($watchJoined) {
    # dir 名は引数から作るため、パス区切りは無害化する (名前に / \ は現れない想定の防御)
    $sub = ($watchJoined -replace ",", "+") -replace '[/\\]', "_"
    $dir = Join-Path $base $sub
    $mirrorArgs = @("live-mirror", "--out-dir", $dir, "--watch", $watchJoined)
} else {
    $dir = Join-Path $base "all"
    $mirrorArgs = @("live-mirror", "--out-dir", $dir)
}
New-Item -ItemType Directory -Force -Path $dir | Out-Null

# レート表キャッシュがあれば併記 (floodgate 運用機で stats.sh を回していれば存在する)
$playerArgs = @("--csa", $dir, "--live", "5")
$ratings = if ($env:FLOODGATE_RATINGS) { $env:FLOODGATE_RATINGS } else { Join-Path $HOME "floodgate/records/ratings_cache.tsv" }
if (Test-Path $ratings) { $playerArgs += @("--ratings", $ratings) }

# ミラーは裏で回す。ログは dir 内 (kifu_player は *.csa しか読まないので混ざらない)。
# Start-Process -ArgumentList は要素をクォートしない実装があるため (Windows PowerShell 5.1)、
# 空白入りパスでも壊れないよう全要素を自前でクォートして 1 本の文字列にする。
$quotedArgs = ($mirrorArgs | ForEach-Object { '"{0}"' -f ($_ -replace '"', '\"') }) -join " "
$log = Join-Path $dir "live-mirror.log"
$mirror = Start-Process -FilePath $pipeline -ArgumentList $quotedArgs `
    -RedirectStandardOutput $log -RedirectStandardError "$log.err" -NoNewWindow -PassThru
try {
    & $player @playerArgs
    $status = $LASTEXITCODE
} finally {
    if (-not $mirror.HasExited) { Stop-Process -Id $mirror.Id -Force }
}
exit $status
