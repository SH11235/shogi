# dump_effect_bucket_golden

`dump_effect_bucket_golden` は、HalfKaHmMerged + `EffectBucket=` modifier の active index を形式一致 golden として出力する開発用ツールです。

固定 SFEN 群について、先手視点・後手視点それぞれの effect bucket active index を config 別に sorted dump します。net の学習/export 形式と同じ入力・同じ形式で出力し、diff が空になることで effect bucket index 合成と bucket 判定の bit 一致を確認します。

## 実行例

```bash
cargo run -p tools --bin dump_effect_bucket_golden
```

## 出力形式

```text
<sfen_no> <B|W> <config_name> : <idx> <idx> ...
```

`config_name` は以下です。

| config | 説明 |
|--------|------|
| `effect_bucket_2x2_kingfixed` | 2x2 bucket、玉は bucket 0 固定 |
| `effect_bucket_2x2_kingbucketed` | 2x2 bucket、玉も bucket 化 |
| `effect_bucket_3x3_kingfixed` | 3x3 bucket、玉は bucket 0 固定 |
| `effect_bucket_3x3_kingbucketed` | 3x3 bucket、玉も bucket 化 |

## 用途

- net の学習/export 形式との HalfKaHmMerged + `EffectBucket=` active index 一致確認
- bucket 量子化、玉 bucket 設定、packed BonaPiece 域判定の regression 検出
- effect bucket feature set 変更時の golden 更新前確認
