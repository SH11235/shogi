#!/bin/bash
# floodgate 記録を kifu_player TUI で見る。
#   tui.sh                              → JSONL を live 追従 + レート併記で開く
#   tui.sh --csa                        → CSA 棋譜側を開く
#   tui.sh <kifu_player の追加引数...>  → そのまま渡る
# 要 PR #882 以降の kifu_player (--live 対応)。それ以前のビルドは floodgate 記録を
# 開けない (--csa 無し・_vs_ 名 JSONL 非対応) ため、明示エラーで再ビルドを促す。
set -euo pipefail
REPO="${RSHOGI_REPO:-$HOME/development/rshogi}"
BIN="${KIFU_PLAYER:-$REPO/target/release/kifu_player}"
REC="${FLOODGATE_RECORDS:-$HOME/floodgate/records}"

if [ ! -x "$BIN" ]; then
  echo "error: $BIN がありません。先にビルドしてください:" >&2
  echo "  cargo build --release -p tools --bin kifu_player" >&2
  echo "  (repo: $REPO。場所が違う場合は RSHOGI_REPO で指定)" >&2
  exit 1
fi
if ! "$BIN" --help 2>/dev/null | grep -q -- --live; then
  echo "error: $BIN が旧ビルドで floodgate 記録を開けません。" >&2
  echo "floodgate 停止時に再ビルドしてください:" >&2
  echo "  cargo build --release -p tools --bin kifu_player" >&2
  exit 1
fi

extra=(--live 3)
[ -f "$REC/ratings_cache.tsv" ] && extra+=(--ratings "$REC/ratings_cache.tsv")
if [ "${1:-}" = "--csa" ]; then
  shift
  exec "$BIN" --csa "$REC" "${extra[@]}" "$@"
fi
exec "$BIN" --tournament-dir "$REC/jsonl" "${extra[@]}" "$@"
