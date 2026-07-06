#!/bin/bash
# floodgate 戦績集計 (floodgate_record)。dir / 自分の名前 / レートキャッシュ・履歴は
# config (CSA_CLIENT_CONFIG、既定 ~/floodgate/active.toml) から導出。追加引数はそのまま渡す:
#   stats.sh
#   stats.sh --watch test_mypeta,Sora_Ginko,nshogi
set -euo pipefail
REPO="${RSHOGI_REPO:-$HOME/development/rshogi}"
BIN="${FLOODGATE_RECORD:-$REPO/target/release/floodgate_record}"
export CSA_CLIENT_CONFIG="${CSA_CLIENT_CONFIG:-$HOME/floodgate/active.toml}"

if [ ! -x "$BIN" ]; then
  echo "error: $BIN がありません。先にビルドしてください:" >&2
  echo "  cargo build --release -p tools --bin floodgate_record" >&2
  echo "  (repo: $REPO。場所が違う場合は RSHOGI_REPO で指定)" >&2
  exit 1
fi
exec "$BIN" --fetch-ratings --ratings-max-age 21600 "$@"
