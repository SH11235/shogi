# floodgate_record

`csa_client` が per-game に出力する JSONL(`meta` / `move` / `result` 行)から、
あるエンジンの戦績を集計する CLI。相手が毎局変わる floodgate 連続対局のログ群を、
先後別・相手別・非勝一覧・実戦 NPS で要約する。

`analyze_selfplay` が 2 エンジンの tournament(Elo/SPRT)を対象にするのに対し、本ツールは
「1 エンジン vs 多数の相手」を集計する点が異なる。

## 入力

`--dir` 配下の `*.jsonl`(1 局 1 ファイル、`csa_client` の `[record] save_jsonl=true` 出力)。
各局の判定に使う行:

- `move` 行: `engine`(指した側の名前)/ `ply` / `eval.nps` / `eval.time_ms`。
  **両対局者の手が engine 名付きで記録される**ため、`ply==1` の `engine` が先手、もう一方が後手。
  後手が 1 手も指さず負けた局だけは move に相手が出ないので、その相手はファイル名
  `..._vs_{gote}` から補完する(取りこぼし防止)。
- `result` 行: `winner`(勝者名。引き分けは無し)/ `reason` / `plies`。

`result` 行が無いファイル(進行中の局)は自動でスキップする。

## 使い方

```bash
# 対象エンジンは自動判定(全局に最も多く出現するエンジン=自分)
floodgate_record --dir ~/floodgate/records/jsonl

# 対象エンジンを明示し、注目相手(部分一致)を指定
floodgate_record --dir ./jsonl --me RAMU_TF --watch Suisho,dlshogi,nshogi

# 自分/相手の現在レートを wdoor から取得して併記(キャッシュ併用で障害時フォールバック)
floodgate_record --dir ~/floodgate/records/jsonl --fetch-ratings --ratings-cache ratings.tsv
```

| オプション | 説明 |
|-----------|------|
| `--dir <PATH>` | 集計対象 JSONL のあるディレクトリ(再帰なし、既定 `.`) |
| `--me <NAME>` | 集計対象エンジン名。省略時は全局に最も多く出現するエンジンを自動判定 |
| `--watch <NAMES>` | 注目相手(カンマ区切り・部分一致)。指定時のみ「注目相手との対戦」節を出力 |
| `--fetch-ratings` | wdoor floodgate の現在レート表を**起動の度に**取得し、自分/相手に ` (R<rate>)` を併記(要ネットワーク)。wdoor は per-game レートを出さないので**現在値**(対局時点ではない)。取得・解析は `tools::common::floodgate` を再利用。取得失敗時はレート併記だけ諦め、集計は続行 |
| `--ratings-cache <FILE>` | `--fetch-ratings` 併用。fetch 成功時は `name<TAB>rate` を FILE へ書き出し、失敗時は FILE を読み戻してフォールバック併記する(一時障害でも直近値を維持) |

レート併記は**名前の完全一致**で引く(表に無い/名前不一致の相手は併記なし)。

## 出力

- 通算 W-L-D と勝率(全体 / 引分除く)
- 先手番 / 後手番 別の W-L-D と勝率(最上位帯は先手ほぼ必勝・後手勝ちの価値が大きい)
- 対象エンジンの実戦 NPS(`time_ms>=500` の本探索の median)
- 相手別 W-L-D
- 後手勝ち一覧(相手名・reason・手数。`--watch` 該当相手は `★上位AI` 表示)
- 負け一覧 / 引分一覧(それぞれ手番・相手・reason・手数)
- `--watch` 指定時: 注目相手との対戦一覧

勝敗判定は `result.winner` を基準にする(`winner==me`→勝ち、他名→負け)。winner が無い局は
`reason` で判別し、`sennichite` / `max_moves` / `jishogi` のみ引き分けに数える。中断・検閲・error
など未完了局(csa_client は `outcome="draw"` / `reason="interrupted"` で書く)は勝率を歪めないよう
**集計対象外**。対象エンジンが参加していない局(別ハンドルのログ混在)も除外する。除外があれば
ヘッダに件数(対象不参加 / 中断・未完了)を表示する。
