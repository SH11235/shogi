# ek_testset

`ek_testset` は、held-out CSA 棋譜から入玉局面の静的評価テストセットを作り、native NNUE 評価で採点するツールです。

局面には 2 種類のラベルを付け、それぞれ別の指標系で採点します:

- **DT (Declaration Truth / 宣言真値)**: `Position::declaration_win` が「手番側の宣言勝ち成立」と
  判定した局面に付く「手番側勝ち」ラベル。ルールベースの判定なので ground truth として扱える。
  評価関数が勝ち確定局面を正しくプラスに読めるかを測る。
- **OC (Outcome Calibration / 勝敗較正)**: 実対局の最終結果を手番側視点の win/loss/draw にしたラベル。
  評価値から作る予測勝率 `sigmoid(eval/scale)` が実スコアとどれだけ整合するか（較正）を測る。
  draw は入玉の変換失敗（千日手・持将棋）の署名そのものなので既定で採点対象に含め、
  期待スコア 0.5 として cross entropy / Brier / calibration に算入する（符号一致率は勝敗のみ）。

## build

```bash
cargo run -p tools --release --bin ek_testset -- build \
  --input "$HOME/data/floodgate/extracted/band" \
  --out-dir runs/ek_testset/band \
  --min-ply-from-entry -20 \
  --sample-stride 4
```

出力:

| ファイル | 内容 |
|---|---|
| `testset.jsonl` | 1 行 1 局面。`sfen`, `stm`, `ply`, `source_csa`, `is_declarable`, `dt_label`, `oc_label`, `floodgate_eval_cp` |
| `sfens.txt` | `testset.jsonl` と同順の SFEN |
| `meta.json` | 入力、件数、パラメータ（生成元 CSA はレコードごとの `source_csa` に記録） |

core が `entering_king_point_info` を公開していないため、点数系フィールド
（`points_stm`, `king_in_enemy_stm`, `enemy_zone_pieces_stm`）は出力しません。DT ラベルは
`Position::declaration_win(EnteringKingRule::Point27)` で作ります。

## eval

```bash
cargo run -p tools --release --bin ek_testset -- eval \
  --testset runs/ek_testset/band/testset.jsonl \
  --eval-file "$SHOGI_DATA/nnue/model.bin" \
  --progress-file "$SHOGI_DATA/progress/progress.bin" \
  --out runs/ek_testset/band/metrics.json
```

標準出力と `--out` に同じ JSON を出します。評価値は手番側視点 cp として扱い、DT は宣言可能局面の
符号一致率と +600cp 超過率、OC は実対局結果との符号一致率（勝敗のみ）、cross entropy、Brier、
予測勝率を [0,1] で 10 等分した等幅ビンの calibration（ビンごとの平均予測勝率と実スコア率。
draw は 0.5 として算入）を出します。calibration は全予測を保持せずビンごとの逐次集計で求めるため、
ピークメモリは入力件数に非依存です。符号一致率では `eval == 0` を一致に含めず、DT/OC とも
不一致として扱います。

`--scale`（既定 600 = いわゆるポナンザ定数）は cp→予測勝率の変換定数です。600 が現代の
評価関数に対して適切かは議論があるため絶対較正の解釈には注意し、モデル間・学習前後の比較では
**同一の scale に固定**して差分を見ます（符号一致率と DT 指標は scale に依存しません）。
cross entropy を最小化する scale を数点スキャンして実測 fit するのが確実です。
