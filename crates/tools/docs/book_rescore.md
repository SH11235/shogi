# book_rescore

`book_rescore` は YANEURAOU-DB2016 テキスト定跡 `.db` の各候補手に、USI エンジンの探索評価値を付け直すツールです。

入力定跡の局面・採択回数は保持し、候補手ごとの `value` と `depth` を更新します。出力順は決定的で、局面は SFEN 昇順、指し手は `count` 降順から USI 昇順です。

## 使い方

```bash
cargo run -p tools --release --bin book_rescore -- \
  --book input.db \
  --out rescored.db \
  --engine /path/to/usi-engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --go "nodes 100000" \
  --journal rescored.jsonl \
  --report rescored_report.tsv \
  --parallel 1
```

## 主なオプション

| オプション | 説明 |
|------------|------|
| `--book <path>` | 入力 `.db` |
| `--out <path>` | value/depth 更新後の出力 `.db` |
| `--engine <path>` | USI エンジンの実行ファイル |
| `--engine-option <k=v>` | USI option。複数指定可 |
| `--go "<args>"` | `go` に渡す引数。例: `nodes 100000`, `depth 15` |
| `--journal <path.jsonl>` | 探索結果の追記 journal |
| `--resume` | journal 済み局面を再探索しない |
| `--report <path>` | TSV レポート出力 |
| `--parallel <N>` | 並列エンジン数。既定 1 |
| `--no-parent-search` | 親局面探索を無効化。既定有効 |

## value の規約

各候補手は、親局面にその手を適用した子局面を探索して評価します。

```text
value(move) = -(子局面の score cp)
```

これにより `.db` の `value` は親局面の手番側視点になります。`score mate N` は `±(30000 - |N|)` の cp に変換し、cp 値は `[-30000, 30000]` にクリップします。

## journal と決定性

出力 `.db` の局面行は、入力の full SFEN（末尾 ply を含む）単位で保持します。同じ盤面・手番でも末尾 ply が異なる `sfen` 行は別 entry として出力されます。

探索 cache のキーは ply を除いた SFEN です。同じ親局面や同じ子局面に合流する候補手は 1 回だけ探索され、結果は journal に追記されます。

journal は JSON Lines 形式です。各行には `kind`, `sfen`, `go`, `engine_fingerprint` と、子局面なら `value`/`depth`、親局面なら `bestmove` を記録します。`engine_fingerprint` は `--engine` パスの basename と、`--engine-option` を key 昇順に正規化した文字列から作ります。

`--resume` は `go` と `engine_fingerprint` の両方が現在の実行設定と一致する journal 行だけを再利用します。`--engine` や `--engine-option`（例: `EvalFile`）を変えた場合、同じ journal ファイル内の古い行は stale として無視され、再探索されます。

USI エンジンの探索結果自体は実行環境により揺れる可能性があります。再現性が必要な場合は、同じ journal と同じ探索設定で `--resume` を使ってください。同一 journal から生成する `.db` と report の内容・順序は決定的です。

## report

`--report` を指定すると TSV を出力します。

- 親局面ベース: エンジン bestmove が定跡候補に含まれる率、count 筆頭手と一致する率
- 指し手ベース: 局面内の `value_top - value` が 100/200/300cp 以上の手数
- 全体、先手番局面、後手番局面を分けて集計
