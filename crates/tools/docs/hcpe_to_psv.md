# hcpe_to_psv

`hcpe_to_psv` は、hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード）を
PSV（YaneuraOu PackedSfenValue, 40B/レコード）へ変換するツールです。

外部公開の hcpe 教師/検証プール（例: dlshogi 系で標準の floodgate 検証局面）を、
nnue-train の `--data` / `--test-data` が読む PSV 形式にするのが主用途です。

## 使用例

```bash
# 単一ファイル
cargo run --release -p tools --bin hcpe_to_psv -- \
  --input "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.hcpe" \
  --output "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.psv"

# ディレクトリ内の .hcpe を一括変換（パスのソート順で連結）
cargo run --release -p tools --bin hcpe_to_psv -- \
  --input-dir "$SHOGI_DATA/teachers/pool" --output out.psv
```

## オプション

| オプション | 説明 | 既定値 |
|---|---|---:|
| `--input <FILES>` | 入力 hcpe（カンマ区切りで複数可）。`--input-dir` と排他 | - |
| `--input-dir <DIR>` | 入力ディレクトリ。`--pattern` と組み合わせる | - |
| `--pattern <GLOB>` | `--input-dir` 使用時の glob パターン | `*.hcpe` |
| `--output <FILE>` | 出力 PSV ファイル | - |

## フィールド対応

| hcpe | PSV | 変換 |
|---|---|---|
| `hcp` (HuffmanCodedPos, Apery/cshogi 形式) | `sfen` (PackedSfen, YaneuraOu 形式) | Huffman テーブルが異なるため `Position` 経由で再パック |
| `eval` (手番側視点 cp) | `score` | そのままコピー（両形式で同一規約） |
| `bestMove16` (cshogi Move16) | `move16` (YaneuraOu Move16) | 駒打ちの駒種 index が 1 ずれるため再エンコード |
| `gameResult` (絶対視点 0=draw / 1=black_win / 2=white_win) | `game_result` (手番側視点 1=win / -1=loss / 0=draw) | 手番で符号を決定 |
| （なし） | `game_ply` | hcpe に手数情報が無いため 1 固定 |

## 挙動

- 入力順（複数ファイルはパスのソート順）を保持して連結出力する。決定的。
- streaming 処理でピークメモリは入力サイズに非依存。
- 壊れたレコード（hcp デコード失敗 / bestMove16 不正 / gameResult が 0/1/2 以外）は
  skip して種別ごとに件数を summary に出す。
- 入力サイズが 38 の倍数でないファイルは即エラー。
