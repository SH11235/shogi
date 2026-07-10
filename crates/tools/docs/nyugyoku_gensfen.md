# nyugyoku_gensfen — 入玉 gensfen 開始局面抽出

floodgate CSA 棋譜の manifest から、玉が敵陣へ初侵入した手数を基準に入玉開始局面を抽出し、
gensfen の `--startpos-file` にそのまま渡せる `startpos.txt` を作る。

## クイックスタート

### 1. 抽出する

```bash
cargo run -p tools --release --bin nyugyoku_gensfen -- \
  --manifest /path/to/manifest.tsv \
  --out-dir  /path/to/out
```

成功すると `/path/to/out/` に以下が生成される。`out/` は全処理が成功した時だけ出現し、
実行中・失敗時は sibling の `/path/to/out.work/` に作業データが残る（再開可能、後述）。

- `startpos.txt` — `gensfen --startpos-file` にそのまま渡せる
- `provenance.tsv` — 各開始局面の出典（棋譜・アンカー手・評価値）
- `run-meta.json` — 件数・partition 数などの完了メタデータ

### 2. gensfen に渡す

```bash
cargo run -p tools --release --bin gensfen -- \
  --startpos-file /path/to/out/startpos.txt \
  --eval-file /path/to/eval.bin
  # 対局条件などは gensfen.md を参照
```

`startpos.txt` の 1-origin 行番号は gensfen result JSONL の `start_pos_index` に対応し、
`provenance.tsv` の `startpos_line` で開始局面の出典を追える。

### 進捗の読み方

処理は manifest を先頭から流し、stderr に警告を出しながら進む。以下は異常終了ではなく、
対象行を飛ばして続行している合図:

- **CSA 読込・パース失敗**: その行を warn+skip して続行する（整合検証用の digest には
  読めた内容だけを積むため、skip があっても resume 整合は保たれる）
- **total_plies 食い違い**: manifest の手数と CSA 実手数がずれた棋譜。警告を出し、
  実手数を超えるアンカーを除外して続行する

manifest 自体の形式異常だけはエラーで停止する。既定 100 万行ごと・10 分ごと・
各 dedup partition 完了時に `out.work/state.json` へ checkpoint が保存される。

### 中断と再開

中断したら、同じ引数に `--resume` を足して再実行する。再開は checkpoint 時点から続く。

```bash
cargo run -p tools --release --bin nyugyoku_gensfen -- \
  --manifest /path/to/manifest.tsv \
  --out-dir  /path/to/out \
  --resume
```

**既処理部分の manifest 書き換え・参照先 CSA の内容変更・`--partitions` /
`--legacy-server-eval-comments` の変更があると再開は拒否される**（partition 処理中の
manifest 末尾への追記だけは可。dedup 開始後は行数追加も拒否）。`--resume` 開始時は
既処理 prefix と参照 CSA を先頭から再検証するため巨大 corpus の終盤では時間がかかるが、
CSA 解析・局面再生・partition 書込みはやり直さない。この再検証自体は checkpoint の
対象外で、再開のたびに先頭からやり直す。公開直前に停止した場合も
`run-meta.json` を checkpoint として `--resume` できる。

## 入力（manifest）

タブ区切り 4 列:

```text
csa_path	black_entry_ply	white_entry_ply	total_plies
```

`entry_ply` は玉が敵陣三段へ初侵入した手数。未侵入は `-1`。相対 `csa_path` は
manifest があるディレクトリを基準に解決する。

## 抽出・dedup ルール

各侵入イベントごとに `{entry-40, entry-20, entry, entry+20}` を候補にし、
`16 <= anchor <= total_plies - 8` を満たすものだけ採用する。「アンカー後に 8 手以上残る」
の保証は manifest の `total_plies` だけでなく CSA パーサの実手数にも課す。CSA を先頭から
アンカー手数まで再生し、その局面で手番側が 27 点法の宣言勝ち可能なら除外する。

盤面・手番・持ち駒が同じ局面は、SFEN の手数が異なっても同一として exact dedup し、
manifest で先に出たものを残す。

## オプション

| フラグ | 既定 | 説明 |
|---|---|---|
| `--manifest <PATH>` | （必須） | 入力 manifest（TSV 4 列） |
| `--out-dir <DIR>` | （必須） | 出力先。既存ディレクトリがあると起動エラー |
| `--partitions <N>` | 128 | dedup のディスクパーティション数（1〜4096）。ピークメモリのレバー（後述） |
| `--resume` | false | `<out-dir>.work` の checkpoint から再開 |
| `--checkpoint-interval <N>` | 1000000 | checkpoint を保存する manifest 処理行数間隔 |
| `--legacy-server-eval-comments` | false | 手の後へ `'*` 評価コメントを書く旧形式の rshogi-csa-server 棋譜として解釈 |

`--legacy-server-eval-comments` は 1 回の manifest 内で標準形式と旧 server 形式を
混在させず、形式ごとに別 run へ分けて使う。

## 出力

`provenance.tsv` の列:

```text
startpos_line	source_csa	anchor_ply	anchor_kind	entry_side	anchor_move_eval_cp_black	total_plies	source_year
```

`startpos_line` は `startpos.txt` の 1-origin 行番号。出力順は partition 番号順で、
同じ入力・同じ `--partitions` なら bit 単位で決定的になる。

`anchor_move_eval_cp_black` は、CSA の `'* <cp> ...`（直後の指し手に帰属）と
`'** <cp> ...`（直前の指し手に帰属）の評価コメントから取った、アンカー手を探索した
局面の**先手視点**評価値。出力 SFEN の静的評価値ではない。

## 運用上の注意（リソース見積もり）

- **メモリ**: dedup のピークメモリは全候補数ではなく「最大 partition のユニーク局面数」で
  決まる。ユニーク SFEN キー 500 万件の実測（2026-07-10）では、全件 `HashSet<String>`
  831,592 KiB / 8.31 秒に対し、128 分割して 1 partition ずつ処理すると 9,416 KiB /
  1.77 秒（dedup set 部分のみの比較で、CSA 解析や disk I/O は含まない）。数億〜十億
  候補の規模では `--partitions` を増やして 1 partition あたりのユニーク数を抑える。
- **一時ディスク**: 候補数と source path を含む JSONL レコード長に比例し、1 億候補で
  数十 GiB 以上になり得る。本番投入前に小さい代表 manifest で `out.work/partitions` の
  1 候補あたり byte 数を測り、同一 filesystem へ十分な空きを確保する。
- **成果物の公開**: 最終 `out/` は全処理が成功した時だけ現れ、既存の `out/` を
  上書きしない。実行中・失敗時は `out.work/` が再開用に残る。完成した `out/` に
  `state.json` は含まれない。

## 内部動作（参考）

候補は position key の安定ハッシュで `out.work/partitions/` の各ファイルへ振り分け、
partition 単位でだけ `HashSet` へロードして dedup する（partition ごとに 64 KiB の
書込みバッファを持ち — 既定 128 partition で合計 8 MiB — file descriptor は
partition 数に比例して保持しない）。
checkpoint では変更のあった partition と一時出力を flush・sync し、byte 位置と
継続可能な FNV-1a 64bit checksum を `state.json` に保存する（checksum は偶発的破損の
best-effort 検出用で、暗号学的 digest ではない）。resume 時は既処理 manifest prefix と
参照 CSA 内容の累積 SHA-256 を照合し、checkpoint より後の一時データは記録済み byte 位置へ
切り戻してから再処理する。公開は `out.work/` から同一 filesystem 内の rename
（Linux では `RENAME_NOREPLACE`）で行う。
