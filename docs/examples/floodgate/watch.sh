#!/bin/bash
# wdoor floodgate の対局を 1 コマンドでライブ観戦する。
# 裏で floodgate_pipeline live-mirror を起動し、前面で kifu_player TUI を開く。
# TUI を閉じる (q) とミラーも自動停止する。
#   watch.sh                     → 当日の全対局
#   watch.sh 名前[,名前...]      → 指定 AI の対局のみ (対局者名の部分一致)
# ミラー dir は既定 ~/floodgate-mirror/<対象>/ に残る (後から見返せる)。
set -euo pipefail
REPO="${RSHOGI_REPO:-$HOME/development/rshogi}"
PIPELINE="${FLOODGATE_PIPELINE:-$REPO/target/release/floodgate_pipeline}"
PLAYER="${KIFU_PLAYER:-$REPO/target/release/kifu_player}"
BASE="${FLOODGATE_WATCH_DIR:-$HOME/floodgate-mirror}"

# ミラーは裏で起動するためバイナリ不在のエラーがログ行きになり原因が見えない。先に検証する。
for bin in "$PIPELINE" "$PLAYER"; do
  if [ ! -x "$bin" ]; then
    echo "error: $bin がありません。先にビルドしてください:" >&2
    echo "  cargo build --release -p tools --bin kifu_player --bin floodgate_pipeline" >&2
    echo "  (repo: $REPO。場所が違う場合は RSHOGI_REPO で指定)" >&2
    exit 1
  fi
done

watch="${1:-}"
if [ -n "$watch" ]; then
  # dir 名は引数から作るため、パス区切りは無害化する (名前に / は現れない想定の防御)
  sub="${watch//,/+}"
  sub="${sub//[\/\\]/_}"
  dir="$BASE/$sub"
  mirror_args=(--watch "$watch")
else
  dir="$BASE/all"
  mirror_args=()
fi
mkdir -p "$dir"

# レート表キャッシュがあれば併記 (floodgate 運用機で stats.sh を回していれば存在する)
player_args=()
ratings="${FLOODGATE_RATINGS:-$HOME/floodgate/records/ratings_cache.tsv}"
[ -f "$ratings" ] && player_args+=(--ratings "$ratings")

# ミラーは裏で回す。ログは dir 内 (kifu_player は *.csa しか読まないので混ざらない)。
# 空配列の展開は ${arr[@]+...} でガード (bash 4.4 未満の set -u は空配列展開でエラー)。
"$PIPELINE" live-mirror --out-dir "$dir" ${mirror_args[@]+"${mirror_args[@]}"} \
  >"$dir/live-mirror.log" 2>&1 &
mirror_pid=$!
trap 'kill "$mirror_pid" 2>/dev/null || true' EXIT

exec_status=0
"$PLAYER" --csa "$dir" --live 5 ${player_args[@]+"${player_args[@]}"} || exec_status=$?
exit "$exec_status"
