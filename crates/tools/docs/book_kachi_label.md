# book_kachi_label

`book_kachi_label` は YANEURAOU-DB2016 テキスト定跡 `.db` の各ノード候補手について、
CSA corpus から `%KACHI`（入玉宣言勝ち）決着率を集計し、政策層用 sidecar JSONL を出力します。

```bash
cargo run --release -p tools --bin book_kachi_label -- \
  --book book.db \
  --corpus /path/to/csa \
  --out sidecar.jsonl \
  --min-rating 4000 \
  --report report.md
```

## 入力

| 引数 | 説明 |
|---|---|
| `--book <PATH>` | YANEURAOU-DB2016 テキスト定跡 `.db` |
| `--corpus <DIR>` | CSA root。配下の `*.csa` を再帰列挙し、パス昇順で処理 |
| `--out <PATH>` | sidecar JSONL 出力先 |
| `--min-rating <N>` | 両対局者の対局時レート下限。既定は `4000`。`0` なら無効 |
| `--report <PATH>` | Markdown report 出力先 |

CSA は `rshogi-csa::parse_csa_full` で解析します。レートは CSA コメントの
`'black_rate:` / `'white_rate:` から `GameInfo` に入る値を使い、`--min-rating > 0` では
片方でも欠落または閾値未満なら除外します。

## 集計仕様

- book のノードキーは SFEN 末尾の ply を除いた 3 フィールド（盤面・手番・持ち駒）です。
- CSA の通常手を初手から 1 手ずつ replay し、各手の直前局面キーと指し手 USI を照合します。
- CSA→USI 変換は `rshogi_csa::csa_move_to_usi` を、盤面更新は
  `rshogi_csa::Position::apply_csa_move` を使います。`rshogi-core` 盤面は並走しません。
- 直接キーが book に無い場合だけ、`rshogi_book::flipped_key` で反転キーを引き、
  ヒットした場合は `rshogi_book::flip_usi_move` で指し手も反転して book 座標に合流します。
- `%KACHI` はその特殊手が現れた時点の手番側を宣言側として扱います。flip 合流時は book 座標に
  合わせて宣言側も先後反転します。
- corpus 本文は 1 ファイルずつ読み込んで処理し、保持するのは book 候補集合と集計値です。

## sidecar JSONL

出力は `sfen_key` 昇順、同一局面内は `move` 昇順で決定的です。

```jsonl
{"sfen_key":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -","move":"7g7f","games":1234,"kachi_black":10,"kachi_white":2}
```

| フィールド | 説明 |
|---|---|
| `sfen_key` | ply を除いた book ノードキー |
| `move` | book 座標の候補手 USI |
| `games` | その `(sfen_key, move)` を通過した回数 |
| `kachi_black` | その通過局のうち、book 座標で先手側が `%KACHI` 宣言した回数 |
| `kachi_white` | その通過局のうち、book 座標で後手側が `%KACHI` 宣言した回数 |

宣言率は `(kachi_black + kachi_white) / games` で計算します。

## report

`--report` を指定すると、以下を含む Markdown を出力します。

- CSA ファイル数、レートフィルタ後の集計対象対局数、`%KACHI` 決着数
- book の node / node-move 数、`games>=50` のカバレッジ
- 宣言率の percentile と bucket 分布
- 宣言率上位 20 件の `(sfen_key, move, games, kachi, rate)`

## 決定性

入力パスを昇順ソートし、集計の merge は加算のみ、出力は `BTreeMap` 順で行います。
rayon による並列処理順に依存しないため、同一入力では byte 一致します。
