#!/bin/bash
# floodgate 戦績集計 (floodgate_record)。dir / 自分の名前 / レートキャッシュ・履歴は
# active.toml から導出。追加引数はそのまま渡す:
#   stats.sh
#   stats.sh --watch test_mypeta,Sora_Ginko,nshogi
set -euo pipefail
BIN="${FLOODGATE_RECORD:-$HOME/development/rshogi/target/release/floodgate_record}"
export CSA_CLIENT_CONFIG="${CSA_CLIENT_CONFIG:-$HOME/floodgate/active.toml}"
exec "$BIN" --fetch-ratings --ratings-max-age 21600 "$@"
