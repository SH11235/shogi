# nnue_saturation

LayerStacks NNUE の**活性飽和率**を実局面で計測する診断ツールです。ClippedReLU /
SqrClippedReLU 系の活性は u8 [0,127] に clamp されるため、127 到達率が高いほど
量子化天井で情報が落ちています（評価値インフレ → 隠れ層飽和、の計器）。

計測する 3 段:

| 段 | 内容 |
|---|---|
| `ft` | FT accumulator の因子 clamp(acc, 0, 127) の 127 到達率（両視点 2×L1 因子。SqrClippedReLU の出力は `(a*b) >> 7` で最大 126 のため、飽和は pairing 前の因子側で観測する） |
| `l1_act` | L1→L2 activation（SqrClippedReLU + ClippedReLU の 2×main_dim 要素） |
| `l2_act` | L2→output activation（ClippedReLU の 32 要素） |

**重み側**（i8 dense weight の ±127 張り付き率、FT i16 の飽和接近）は rshogi-nnue
(tatara) の `crates/nnue-format/examples/clamp_stats.rs` が担当します。本ツールは
局面依存の活性側のみを扱います。

## 使い方

```bash
cargo run -p tools --release --bin nnue_saturation -- \
  --nnue "$SHOGI_DATA/nnue/model.bin" \
  --progress-coeff "$SHOGI_DATA/progress/progress.bin" \
  --progress-buckets 8 \
  --sfens sfens.txt \
  --out saturation.json
```

出力は JSON（標準出力と `--out`）。全体集計 `total` と、progress bucket 別の
`per_bucket`（局面が 1 件以上入った bucket のみ）を含みます。各段は
`*_sat` / `*_total` / `*_rate`（127 到達数 / 総数 / 率）です。

入玉局面（大評価値側）と一般局面で `--sfens` を替えて比較すると、評価値インフレが
どの帯で天井に当たっているかを切り分けられます。bucket は progresskpabs なので、
終盤 bucket ほど入玉局面が集中します。
