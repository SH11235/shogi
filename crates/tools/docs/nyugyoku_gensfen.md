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
同一 SFEN は実行全体で dedup し、先に出たものを残す。

CSA の指し手直後に `'** <cp> ...` コメントがあれば、アンカー手の評価値として
`provenance.tsv` に記録する。

## 出力

```text
out/
  startpos.txt      # gensfen --startpos-file にそのまま渡せる
  provenance.tsv    # startpos の出典
```

`provenance.tsv` の列:

```text
id	startpos_line	source_csa	anchor_ply	anchor_kind	entry_side	eval_cp	total_plies	source_year
```

`startpos_line` は `startpos.txt` の 1-origin 行番号で、gensfen result JSONL の
`start_pos_index` と対応する。
