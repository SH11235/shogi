param(
    [Parameter(Mandatory = $true)]
    [string]$Repo,
    [Parameter(Mandatory = $true)]
    [string]$OutDir
)

$ErrorActionPreference = "Stop"
$repoPath = [System.IO.Path]::GetFullPath($Repo)
if ($OutDir -cnotmatch '^(?:[A-Za-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+)') {
    throw "selfplay output gate: --out-dir must be an absolute path; got $OutDir"
}

$worktreeOutput = & git -c "safe.directory=$repoPath" -C $repoPath worktree list --porcelain
if ($LASTEXITCODE -ne 0) {
    throw "selfplay output gate: git worktree list failed"
}

$worktrees = @(
    $worktreeOutput |
        Where-Object { $_.StartsWith("worktree ") } |
        ForEach-Object { [System.IO.Path]::GetFullPath($_.Substring(9)) }
)
if ($worktrees.Count -lt 1) {
    throw "selfplay output gate: git reported no worktrees"
}
$canonical = $worktrees[0]
if ((Split-Path -Leaf $canonical) -cne "rshogi") {
    throw "selfplay output gate: primary worktree must be named 'rshogi'; got $canonical"
}
if (-not (Test-Path -LiteralPath $canonical -PathType Container)) {
    throw "selfplay output gate: primary worktree does not exist: $canonical"
}

$runsRoot = Join-Path $canonical "runs"
$outputRoot = Join-Path $runsRoot "selfplay"
foreach ($persistentPath in @($canonical, $runsRoot, $outputRoot)) {
    if (-not (Test-Path -LiteralPath $persistentPath -PathType Container)) {
        throw "selfplay output gate: persistent path does not exist: $persistentPath"
    }
    $item = Get-Item -LiteralPath $persistentPath -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "selfplay output gate: persistent path must not be a reparse point: $persistentPath"
    }
}

$outputRoot = [System.IO.Path]::GetFullPath($outputRoot).TrimEnd("\", "/")
$outPath = [System.IO.Path]::GetFullPath($OutDir).TrimEnd("\", "/")
$outParent = [System.IO.Path]::GetDirectoryName($outPath).TrimEnd("\", "/")
if (-not $outParent.Equals($outputRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "selfplay output gate: --out-dir must be a direct child of $outputRoot; got $outPath"
}
if (Test-Path -LiteralPath $outPath) {
    throw "selfplay output gate: run directory must not exist before kick: $outPath"
}

$runName = [System.IO.Path]::GetFileName($outPath)
if ($runName -cnotmatch '^\d{8}-\d{6}-[A-Za-z0-9][A-Za-z0-9._-]*$') {
    throw "selfplay output gate: run directory must match YYYYMMDD-HHMMSS-PURPOSE; got '$runName'"
}
$timestamp = $runName.Substring(0, 15)
try {
    [void][datetime]::ParseExact(
        $timestamp,
        "yyyyMMdd-HHmmss",
        [System.Globalization.CultureInfo]::InvariantCulture,
        [System.Globalization.DateTimeStyles]::None
    )
}
catch {
    throw "selfplay output gate: invalid timestamp '$timestamp'"
}

Write-Output "canonical_rshogi=$canonical"
Write-Output "selfplay_out=$outPath"
