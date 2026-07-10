# nyugyoku_gensfen — 入玉 gensfen 開始局面抽出

floodgate CSA 棋譜の manifest から、玉が敵陣へ初侵入した手数を基準に gensfen 用の
開始局面ファイルを作る。候補は disk partition 方式で exact dedup するため、全候補を
メモリへ保持しない。checkpoint/resume と完了時の atomic publish に対応する。

## 入力

```bash
cargo run -p tools --release --bin nyugyoku_gensfen -- \
  --manifest /path/to/manifest.tsv \
  --out-dir /path/to/out
```

manifest はタブ区切り4列:

```text
csa_path	black_entry_ply	white_entry_ply	total_plies
```

`entry_ply` は玉が敵陣三段へ初侵入した手数。未侵入は `-1`。相対`csa_path`は
manifestがあるディレクトリを基準に解決する。

## 抽出・dedupルール

各侵入イベントごとに`{entry-40, entry-20, entry, entry+20}`を候補にし、
`16 <= anchor <= total_plies - 8`を満たすものだけ採用する。CSAを先頭から
アンカー手数まで再生し、その局面で手番側が27点法の宣言勝ち可能なら除外する。

盤面・手番・持ち駒が同じ局面は、SFENの手数が異なっても同一としてexact dedupし、
manifestで先に出たものを残す。候補は安定ハッシュでディスクパーティションへ書き、
各パーティションだけを`HashSet`へロードする。ピークメモリは全候補数ではなく最大
パーティションのユニーク局面数で決まる。既定は128パーティションで、
`--partitions`で変更できる。partitionごとに64 KiBのメモリbuffer（既定で合計8 MiB）を
持ち、満杯時だけ対象ファイルをopen・追記・closeする。checkpointのsyncも1ファイルずつ
行うため、partition数に比例してfile descriptorを保持しない。

### dedupメモリ実測

2026-07-10、同じ形式のユニークSFENキー500万件を生成する`rustc -O`の小さな
ハーネスを`/usr/bin/time`で測定した結果:

| 方式 | peak RSS | 経過時間 |
|---|---:|---:|
| 全件`HashSet<String>`（変更前相当） | 831,592 KiB | 8.31秒 |
| 128分割し1partitionずつ`HashSet` | 9,416 KiB | 1.77秒 |

ハーネスは`board side hand`形式のキーを連番で500万個生成し、分割版は
`index % 128`ごとにsetを作成・解放した。実ツールではこれにCSA解析とpartition
ファイルI/Oが加わるが、dedup setのピークが全件数ではなく最大partitionで決まることを
確認するための測定である。

一時ディスク量は候補数だけでなく、source pathを含むJSONL record長に比例する。1億候補では
数十GiB以上になり得るため、本番投入前に小さい代表manifestで`out.work/partitions`の
1候補あたりbyte数を測り、同一filesystemへ十分な空きを確保する。この表の時間は
disk I/Oを含むend-to-end throughput値ではない。

CSAの`'* <cp> ...`（直後の指し手）と`'** <cp> ...`（直前の指し手）を解釈し、
アンカー手を探索した局面の先手視点評価値をprovenanceへ記録する。

## 中断・再開

実行中の候補、一時出力、checkpointは、`--out-dir /path/to/out`に対してsiblingの
`/path/to/out.work/`へ継続的に書き出す。manifestの既定10,000行ごと、および各dedup
パーティション完了時にdurableなbyte位置を`state.json`へ保存する。checkpointでは変更が
あったpartitionだけをflush・syncし、その後にstateとディレクトリエントリもsyncする。
partitionと一時出力のprefix digestも保存し、同じbyte長の破損・改変も再開時に拒否する。

中断後は同じ引数に`--resume`を加えて再開する。

```bash
cargo run -p tools --release --bin nyugyoku_gensfen -- \
  --manifest /path/to/manifest.tsv \
  --out-dir /path/to/out \
  --resume
```

partition処理中は、処理済みmanifest prefix、そこから参照されるCSA内容の累積SHA-256、
パーティション数が一致しなければ再開を拒否する。未処理の末尾は修正・追記できる。
checkpointより後の一時データは記録済み
byte位置へ切り戻してから再処理する。dedup開始後はmanifest全体の行数と処理済みprefixを
照合し、変更・追記された入力からの再開を拒否する。checkpoint間隔は
`--checkpoint-interval`で変更できる。

最終`out/`は全処理が成功した時だけ、`out.work/`の同一ファイルシステム内renameで
公開される。Linuxでは`RENAME_NOREPLACE`を使い、並行して作られたものを含む既存の`out/`を
上書きしない。実行中・失敗時は`out/`が現れず、`out.work/`が再開用に残る。公開直前に
停止した場合も`run-meta.json`をcheckpointとして`--resume`でき、完成した`out/`に
`state.json`は含めない。

## 出力

```text
out/
  startpos.txt      # gensfen --startpos-file にそのまま渡せる
  provenance.tsv    # startpos の出典
  run-meta.json     # 件数・partition 数などの完了メタデータ
```

`provenance.tsv`の列:

```text
id	startpos_line	source_csa	anchor_ply	anchor_kind	entry_side	anchor_move_eval_cp_black	total_plies	source_year
```

`startpos_line`は`startpos.txt`の1-origin行番号で、gensfen result JSONLの
`start_pos_index`と対応する。出力順はpartition番号順で、同じ入力・同じ
`--partitions`ならbit単位で決定的になる。

`anchor_move_eval_cp_black`は出力SFENの静的評価値ではなく、アンカー手を選んだ探索の
先手視点評価値である。
