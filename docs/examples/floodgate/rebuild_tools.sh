#!/bin/bash
# floodgate 関連バイナリ (csa_client / kifu_player / floodgate_record /
# floodgate_pipeline) を最新ソースで再ビルドする。
#
# csa_client (floodgate) 稼働中は、ビルドが対局エンジンと CPU を取り合うため
# 警告して確認を求める (y で続行 / それ以外は中止)。-y / --yes で確認をスキップ。
set -euo pipefail
REPO="${RSHOGI_REPO:-$HOME/development/rshogi}"

assume_yes=0
if [ "${1:-}" = "-y" ] || [ "${1:-}" = "--yes" ]; then
  assume_yes=1
fi

# PID 固定でなく実行時スキャン: floodgate config で動いている csa_client を探す。
# -x でプロセス名の完全一致に絞り、コマンドライン文字列だけが偶然一致する別プロセス
# (エディタや本スクリプト自身の起動シェル等) を拾わない。
running="$(pgrep -ax csa_client | grep floodgate || true)"
if [ -n "$running" ]; then
  echo "!! floodgate 用 csa_client が稼働中です:" >&2
  echo "$running" >&2
  echo "   負荷: $(uptime | sed 's/.*load average/load average/')" >&2
  echo "   ビルドは全コアを使い、対局中なら持ち時間に影響しえます。" >&2
  echo "   安全なのは kill -INT で停止 (現局完了後 graceful) してからの再実行です。" >&2
  if [ "$assume_yes" -eq 1 ]; then
    echo "-> --yes 指定のため続行します。" >&2
  else
    read -r -p "このまま続行しますか? [y/N] " ans
    case "$ans" in
      y|Y) ;;
      *) echo "中止しました。" >&2; exit 1 ;;
    esac
  fi
fi

cd "$REPO"
git pull --ff-only
cargo build --release -p rshogi-csa-client --bin csa_client
cargo build --release -p tools --features kifu-player \
  --bin kifu_player --bin floodgate_record --bin floodgate_pipeline
echo "done: $REPO/target/release/{csa_client,kifu_player,floodgate_record,floodgate_pipeline}"
