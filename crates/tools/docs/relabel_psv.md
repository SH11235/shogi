# relabel_psv — PSV 勝敗ラベルへの score 置換

`PackedSfenValue` の `score` を、各レコードの `game_result` から得た飽和値へ置換する。
`game_result` と `score` はどちらもその局面の手番側視点で、勝ちを正、負けを負、引分を 0 とする。

処理は40バイトの PSV レコードを逐次読み書きする。入力 PSV の全件をメモリへ保持しない。
複数入力や glob は辞書順へ固定して処理するため、同じ入力とオプションからは同じ byte 列を得る。

## 基本用法

```bash
cargo run -p tools --release --bin relabel_psv -- \
  --input "$SHOGI_DATA/teachers/input-*.psv" \
  --output "$SHOGI_DATA/teachers/relabelled.psv"
```

既定の `--win-cp 2500` では次のように必ず上書きする。

| `game_result` | 新しい `score` |
|---:|---:|
| `1` | `+2500` |
| `0` | `0` |
| `-1` | `-2500` |

`--win-cp` は `1..32000` の範囲で指定する。これにより score-drop の番兵値 32000 と干渉しない。

## 宣言勝ち override

`--declaration-override` を指定すると各 PackedSfen を復号し、手番側が27点法で宣言勝ち可能な
局面の `score` を、対局結果にかかわらず `+win-cp` にする。復号と局面構築が必要なため既定は無効。

## diversions による deblunder

`gensfen --emit-game-id-sidecar` で作った sidecar と result JSONL を使い、乱択着手までの
汚染された局面を出力から除外できる。result JSONL の diversion ply は開始局面からの相対手数で、
`relabel_psv` が各 result 行の `start_sfen` に含まれる開始絶対手数を使って PSV の絶対
`game_ply` に変換する。

```bash
cargo run -p tools --release --bin relabel_psv -- \
  --input "$SHOGI_DATA/teachers/gensfen.psv" \
  --output "$SHOGI_DATA/teachers/gensfen-relabelled.psv" \
  --deblunder \
  --game-id-sidecar "$SHOGI_DATA/teachers/gensfen.game_ids.bin" \
  --diversions "$SHOGI_DATA/teachers/gensfen.jsonl"
```

PSV と sidecar は lockstep で読み、件数不一致や末尾の半端レコードをエラーにする。
sidecar は PSV 1 レコードにつき u32 little-endian の `game_id` 1件である。

| モード | 除外条件 | 既定 |
|---|---|---|
| `drop-before-last` | 最後の diversion 以前（diversion が指された局面を含む）を除外し、厳密に後ろだけ残す | yes |
| `drop-before-any` | 最初の diversion 以前（diversion が指された局面を含む）を除外する | no |

境界 ply 自体も、乱択後の game result で汚染されるため除外する。diversion のない対局は除外しない。
diversions の map だけを対局数オーダーで
保持するため、ピークメモリは PSV レコード数に依存しない。

## 統計

stderr に入力件数、勝ち・負け・引分の置換件数、宣言勝ち override 件数、deblunder の除外件数を
1行で出力する。
