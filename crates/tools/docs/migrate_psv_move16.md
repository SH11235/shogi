# migrate_psv_move16

旧リポジトリ内部形式 (B) の PSV move16 を、実 YaneuraOu 形式
(A: bit14=駒打ち、bit15=成り) へ焼き直す one-shot ツールです。
40バイトレコードを順次処理するため、ピークメモリは入力件数に依存しません。

```bash
cargo run --release -p tools --bin migrate_psv_move16 -- \
  --input old.psv --output migrated.psv
```

| オプション | 既定 | 説明 |
|---|---|---|
| `--input <FILE>` | 必須 | B 形式の単一 PSV ファイル |
| `--output <FILE>` | 必須 | A 形式の出力 PSV ファイル |
| `--verify-legal` | true | PackedSfen から局面を復元し、各 move16 が合法手か検証 |

変換前に先頭10万レコードを調べ、bit15 があれば A 形式、from=81 の駒打ちがあれば
hcpe 形式 (C) の混入として停止します。B 形式の確定シグネチャを確認できない入力も
誤変換防止のため拒否します。

出力は `<output>.partial` に書き、完了後に最終パスへ rename します。move16 の2バイト
以外は入力レコードをそのまま書き出します。合法手検証の不一致はレコード番号を stderr
へ出し、変換を継続して終了時に件数を集計します。
