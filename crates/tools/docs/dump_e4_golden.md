# dump_e4_golden

`dump_e4_golden` は、HalfKa-E4 の active index を cross-repo golden として出力する開発用ツールです。

固定 SFEN 群について、先手視点・後手視点それぞれの E4 active index を config 別に sorted dump します。学習側 repo の同等 dumper と同じ入力・同じ形式で出力し、diff が空になることで E4 index 合成と bucket 判定の bit 一致を確認します。

## 実行例

```bash
cargo run -p tools --bin dump_e4_golden
```

## 出力形式

```text
<sfen_no> <B|W> <config_name> : <idx> <idx> ...
```

`config_name` は以下です。

| config | 説明 |
|--------|------|
| `e4_2x2_kingfixed` | 2x2 bucket、玉は bucket 0 固定 |
| `e4_2x2_kingbucketed` | 2x2 bucket、玉も bucket 化 |
| `kpe9_kingfixed` | 3x3 bucket、玉は bucket 0 固定 |
| `kpe9_kingbucketed` | 3x3 bucket、玉も bucket 化 |

## 用途

- rshogi と学習側 repo の HalfKa-E4 active index 一致確認
- bucket 量子化、玉 bucket 設定、packed BonaPiece 域判定の regression 検出
- E4 feature set 変更時の golden 更新前確認
