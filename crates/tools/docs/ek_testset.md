# ek_testset

`ek_testset` は、held-out CSA 棋譜から入玉局面の静的評価テストセットを作り、native NNUE 評価で採点するツールです。

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
符号一致率と +600cp 超過率、OC は実対局結果との符号一致率、cross entropy、Brier、予測勝率を
[0,1] で 10 等分した等幅ビンの calibration（ビンごとの平均予測勝率と実勝率）を出します。calibration は
全予測を保持せずビンごとの逐次集計で求めるため、ピークメモリは入力件数に非依存です。符号一致率では
`eval == 0` を一致に含めず、DT/OC とも不一致として扱います。
