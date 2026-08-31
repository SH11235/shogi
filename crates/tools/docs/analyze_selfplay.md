# analyze_selfplay

`tournament` 等が出力した JSONL を読み、エンジン別勝敗、直接対決、Elo/nElo、
NPS や深さなどの追加統計を表示する。

## 使い方

```bash
./target/release/analyze_selfplay runs/selfplay/{DIR}/*.jsonl
```

SPRT の post-hoc 判定も表示する場合:

```bash
./target/release/analyze_selfplay --sprt runs/selfplay/{DIR}/*.jsonl
```

`--sprt-base-label` / `--sprt-test-label` で役割を明示できる。省略時は SPRT meta、
`base_label` 記録、ラベル名などから推定し、推定根拠を標準エラー出力に表示する。

Wald パラメータは `--sprt-nelo0` / `--sprt-nelo1` / `--sprt-alpha` /
`--sprt-beta` で上書きできる。

## 表示の視点

- エンジンの `A(...)`, `B(...)` ラベルはエンジン ID の辞書順で決まる。
- 通常の直接対決は辞書順で左側のエンジン視点で、勝率と Elo/nElo の符号を表示する。
- `--sprt` 時の SPRT 対象ペアは `test vs base` の順に表示し、直接対決と SPRT
  レポートをどちらも test 視点に揃える。
- JSON 出力の `head_to_head` は互換性のため従来どおり辞書順を維持する。SPRT の
  `test` / `base` および統計値は test 視点である。

`nElo` と SPRT の pentanomial 集計は、同じ開始局面を先後入れ替えた 2 局を
1 ペアとして扱う。`pair_index` と `attempt` が同じ 2 局だけを組にし、error を含む
世代は正常終了した相方も含めて勝敗・直接対決・SPRT から除外する。片スロットしか
完了していない世代も除外し、件数を情報表示する。`attempt` が無い旧ログは 0 として扱う。

追加統計には `error局`、`errorペア`、`再試行ペア`、`枯渇ペア` を表示する。
`枯渇ペア` が 1 以上なら、そのテストはインフラ障害により invalid である。
