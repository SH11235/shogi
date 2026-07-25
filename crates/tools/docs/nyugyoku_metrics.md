# nyugyoku_metrics

`nyugyoku_metrics` は、大差圏分解能指標 P1「宣言ルール距離ペア順序一致」を測るツールです。
`%KACHI`（入玉宣言勝ち）で終局した対局の勝者手番局面から「宣言成立へのルール距離が確定的に
縮んだ」前後 pair を抽出し（`build-pairs`）、NNUE 静的評価が後局面（距離小）を前局面（距離大）
より高く評価するか＝順序一致率を条件別に測ります（`eval-pairs`）。

既存の ek_testset（DT/OC）が測れない「大差圏内の順序分解能」を、ルール真値のみで
（評価値を母集団選別・真値のどちらにも使わずに）測るのが目的です。

## 用語

- 本ツールが扱うのは**宣言勝ち（`%KACHI`）のみ**です。詰み（checkmate）は扱いません
  （詰み距離は P2 の担当で、本ツールと混ぜません）。
- 「勝者」= 宣言側 = 終端で手番だった側。pair の 2 局面はどちらも勝者手番なので、
  評価値は同一 POV（手番相対）で直接比較できます。

## pair の定義と 4 条件

同一対局の勝者手番局面列から、**ply 差がちょうど 2 の隣接 pair**（間に相手の通常手が
1 手だけ挟まる並び）を取り、以下の遷移条件が 1 つ以上成立したものだけを出力します。
いずれも `Position::entering_king_point_info` / `in_check` によるルールベース判定です:

| 条件 | 判定 |
|---|---|
| `point_gain` | 宣言点（27点法の駒点）が増加 |
| `zone_piece_gain` | 敵陣三段内の駒数（玉を除く）が増加 |
| `king_entry` | 勝者玉が敵陣三段内へ入った（false→true） |
| `check_resolved` | 勝者玉への王手が解除された（true→false） |

間の手が parse fallback（`Move::NONE`）の pair や ply 欠番の pair は、盤面遷移を信頼
できないため出力しません。条件は排他ではなく、1 pair に複数付くことがあります
（`check_resolved` は宣言条件の王手回避と評価の王手ペナルティが交絡しうるため、
条件別の層のまま解釈してください）。また `point_gain` / `zone_piece_gain` の pair は、
同時に悪化遷移（被王手化 `check` false→true や玉の敵陣退出 `king_in` true→false）が
起きていても成立します。これらは除外せず、per-pair の `check_before/after` /
`king_in_before/after` フラグで post-hoc にフィルタして解析できます。

## build-pairs

```bash
cargo run -p tools --release --bin nyugyoku_metrics -- build-pairs \
  --input "$HOME/data/floodgate/extracted/kachi" \
  --out-dir runs/nyugyoku_metrics/kachi
```

CSA（1 ファイル = 1 対局）をファイル名ソートの決定的順序でストリーミング走査します
（ピークメモリは対局数に非依存）。`%KACHI` を含まないファイルは parse せずスキップします。

`%KACHI` 終局でも、以下の対局は警告して除外します（除外数は `meta.json` に計上）:

- 宣言側と終端手番の突き合わせに失敗した対局（`games_skipped_winner_mismatch`）
- replay が完走せず終端局面を復元できない対局（`games_skipped_broken`）
- **終端局面で Point27 宣言が成立しない対局**（`games_skipped_point27_mismatch`）。
  wdoor shogi-server は宣言失敗（illegal kachi = 宣言側の負け）でも棋譜に `%KACHI` を
  書くため、最終手適用後の終端局面（宣言側手番）で `declaration_win`（27点法）を評価し、
  不成立の対局（宣言失敗・別ルール運用）を落とします。

出力:

| ファイル | 内容 |
|---|---|
| `pairs.jsonl` | 1 行 1 pair。`source_csa`, `date_key`（ファイル名由来 `YYYYMMDDHHMMSS`、無ければ null）, `black_engine`/`white_engine`（CSA `N+`/`N-` の生値。将来の時期×エンジン group split 用）, `winner` (`b`/`w`), `ply_before`, `ply_after`, `sfen_before`, `sfen_after`, `conditions`, `points_before/after`, `zone_before/after`, `king_in_before/after`, `check_before/after` |
| `meta.json` | 入力、走査対局数、`%KACHI` 対局数、除外数（上記 3 分類）、総 pair 数、条件別の pair 数と対局数（クラスタ数） |

## eval-pairs

```bash
cargo run -p tools --release --bin nyugyoku_metrics -- eval-pairs \
  --pairs runs/nyugyoku_metrics/kachi/pairs.jsonl \
  --eval-file "$SHOGI_DATA/nnue/model.bin" \
  --progress-file "$SHOGI_DATA/progress/progress.bin" \
  --out runs/nyugyoku_metrics/kachi/metrics.json
```

pair ごとに `sfen_before` / `sfen_after` を NNUE 静的評価（cp）し、順序一致
`eval_after > eval_before` を 1、tie を 0.5、逆転を 0 として、全体と条件別に
一致率・pair 数・対局数を集計します。標準出力と `--out` に同じ JSON を出します。

- **bucket-mode**: `--bucket-mode progress8kpabs`（既定）は `--progress-file` が必須です。
  `--bucket-mode kingrank9` は progress 係数を使わないため `--progress-file` 不要です
  （指定するとエラーにします）。LayerStacks NNUE のみ対応です。
- **95% CI**: 対局クラスタ bootstrap（`source_csa` 単位の復元抽出、既定 `--bootstrap 10000`、
  `--seed 20260726`）で出します。分位点は replicate 統計量を昇順ソートし、0 始まりの
  index `round((R-1)·q)`（R = replicate 数、q = 0.025 / 0.975、round は四捨五入）の要素を
  採ります（nearest-rank 法 `ceil(R·q)-1` とは異なり、線形補間もしません。
  例: R=10000, q=0.025 → index 250）。条件別の bootstrap seed は
  `seed ^ (slot · 0x9E3779B97F4A7C15)` で派生します（全体 slot は seed のまま）。
  同 seed・同入力で結果は bit 一致します（`--bootstrap 0` で CI を省略）。条件ごとの
  クラスタ集合は「その条件の pair を 1 件以上持つ対局」です。実効クラスタ数（`n_games`）
  を必ず併記するので、クラスタが少ない層の CI は幅ではなく `n_games` を見て判断して
  ください。
- **fv_scale への非依存**: FV_SCALE は評価値の定数倍（狭義単調変換）にしか効かないため、
  順序一致率には影響しません（tie 潰れの量子化は測定対象の一部として残ります）。
- `--dump-pairs <path>` で pair ごとの `eval_before` / `eval_after` / `agreement` を
  jsonl 出力できます（deep-dive 用）。

出力 `metrics.json` の形:

```json
{
  "pairs": "...", "eval_file": "...", "progress_file": "...",
  "bucket_mode": "progress8kpabs", "bootstrap": 10000, "seed": 20260726,
  "overall":    { "agreement": 0.7, "n_pairs": 123, "n_games": 40, "ci95_lo": 0.6, "ci95_hi": 0.8 },
  "conditions": {
    "check_resolved":  { "agreement": ..., "n_pairs": ..., "n_games": ..., "ci95_lo": ..., "ci95_hi": ... },
    "king_entry":      { ... },
    "point_gain":      { ... },
    "zone_piece_gain": { ... }
  }
}
```

モデル間の比較は、同一の pairs.jsonl（同一 pair 集合）の上で行ってください。
