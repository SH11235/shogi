#!/bin/bash
# floodgate 関連バイナリ (csa_client / kifu_player / floodgate_record /
# floodgate_pipeline) を最新ソースで再ビルドする。
# 冒頭で `git pull --ff-only` する (repo が main を checkout し upstream 設定済みである
# ことが前提。feature branch 作業中などは失敗して止まる = 意図しない pull はしない)。
#
# csa_client (floodgate) 稼働中は、ビルドが対局エンジンと CPU を取り合うため
# 警告して確認を求める (y で続行 / それ以外は中止)。-y / --yes で確認をスキップ。
set -euo pipefail
REPO="${RSHOGI_REPO:-$HOME/development/rshogi}"

# 非ログインシェル (ssh host 'cmd' 等) では rustup の PATH が通っておらず、
# git pull だけ成功してビルドで落ちる。~/.cargo/env をフォールバックで読む。
if ! command -v cargo >/dev/null 2>&1; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo が見つかりません (rustup 未導入か PATH 未設定)。" >&2
  exit 1
fi

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
cargo build --release -p tools \
  --bin kifu_player --bin floodgate_record --bin floodgate_pipeline
echo "done: $REPO/target/release/{csa_client,kifu_player,floodgate_record,floodgate_pipeline}"
