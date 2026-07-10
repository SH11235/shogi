# nyugyoku_gensfen — 入玉 gensfen 開始局面抽出

floodgate CSA 棋譜の manifest から、玉が敵陣へ初侵入した手数を基準に gensfen 用の
開始局面ファイルを作る。

## 入力

```bash
cargo run -p tools --bin nyugyoku_gensfen -- \
  --manifest /path/to/manifest.tsv \
  --out-dir /path/to/out
```

manifest はタブ区切り 4 列:

```text
csa_path	black_entry_ply	white_entry_ply	total_plies
```

`entry_ply` は玉が敵陣三段へ初侵入した手数。未侵入は `-1`。

## 抽出ルール

各侵入イベントごとに `{entry-40, entry-20, entry, entry+20}` を候補にし、
`16 <= anchor <= total_plies - 8` を満たすものだけ採用する。CSA を先頭から
アンカー手数まで再生し、その局面で手番側が 27 点法の宣言勝ち可能なら除外する。
同一 SFEN は実行全体で dedup し、先に出たものを残す。dedup は gensfen と同じ
direct-mapped 固定サイズテーブル（`--dedup-hash-entries`、2 冪へ切り上げてから確保、
既定 64M エントリ = 512MB）で行い、ピークメモリは入力件数に依存しない。
スロット上書きによる重複検出漏れは使用率（ユニーク局面数 / エントリ数）に比例して増える
（使用率 1 で新規挿入の約 37% が衝突）ため、数億規模では想定ユニーク件数の数倍の
エントリ数を指定する。漏れても重複開始局面が残るだけで実害は軽い。

「アンカー後に 8 手以上残る」の保証は manifest の `total_plies` だけでなく CSA パーサの
実手数にも課す。両者が食い違う棋譜は警告を出し、実手数を超えるアンカーを除外する。
CSA 側の異常（読込失敗・パース失敗）はその行を警告付きで skip し、処理を続行する。
manifest 自体の形式異常はエラーで停止する。

CSA の指し手直後に `'** <cp> ...` コメントがあれば、アンカー手の評価値として
`provenance.tsv` に記録する。コメント行スキャンの手数と CSA パーサの手数が一致しない
棋譜は、誤対応を避けるため eval を記録しない（警告を出す）。

## 出力

```text
out/
  startpos.txt      # gensfen --startpos-file にそのまま渡せる
  provenance.tsv    # startpos の出典
```

`provenance.tsv` の列:

```text
startpos_line	source_csa	anchor_ply	anchor_kind	entry_side	eval_cp	total_plies	source_year
```

`startpos_line` は `startpos.txt` の 1-origin 行番号で、gensfen result JSONL の
`start_pos_index` と対応する。
