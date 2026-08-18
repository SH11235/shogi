# psv_scatter_by_mask — compact PSV の score 書き戻し

`psv_select_by_mask` で抽出し、入力順を保ったまま再スコアした compact PSV の score を、
同じ mask に従って元 shard の対応行へ書き戻します。入力全体をメモリへ載せない
チャンクストリーミング処理です。

## 使い方

```bash
cargo run --release -p tools --bin psv_scatter_by_mask -- \
  --input shard.psv \
  --mask entered.bits \
  --compact relabeled.psv \
  --out out.psv
```

`--out` のレコード数と順序は `--input` と同じです。mask の bit 0 の行は入力レコードを
そのままコピーし、bit 1 の行は対応する compact レコードの score（offset 32--33、i16 LE）
だけを差し替えます。

compact 側の move16、gamePly、game_result、padding は使用しません。再スコア処理が move16
などを更新する場合でも、このツールの目的は score の差し替えだけです。また move16 スロットは
後段の `psv_dual_label embed` が DL score に使用するため、score 以外は元 shard を正とします。

## mask 契約

- LSB-first bitmap。byte `j` の bit `k` は record `j * 8 + k` に対応する
- bit 1 ごとに compact 側を 1 レコード進め、同じ shard 行へ対応させる
- サイズは `ceil(records / 8)` byte と厳密に一致する
- 最終 byte でレコードに対応しない未使用 bit はすべて 0 とする
- mask 全体の popcount は compact PSV のレコード数と厳密に一致する

`psv_select_by_mask` の出力順を変更した compact PSV は使用できません。

## 検証と fail-closed

出力を作成する前に、入力 PSV と compact PSV が 40 byte/record であること、mask 契約、
popcount と compact レコード数の一致を検証します。書き戻し時には各 bit 1 行について、元 shard と
compact の packed SFEN（offset 0--31）が一致することも検証します。不一致時のエラーには元 shard の
0-origin レコード番号を表示します。

出力は `<out>.partial` へ書き、元 shard と同じ期待サイズであることを検査してから `--out` へ
rename します。処理中にエラーが起きた場合は partial を削除し、既存の `--out` を温存します。
`--out` と `<out>.partial` は、`--input`、`--mask`、`--compact` のいずれかと同じパスまたは
同じファイル実体にできません。

## 統計の読み方

正常終了時は次の形式で標準出力へ表示します。

```text
records=1000000 replaced=12345 changed=12001
```

- `records`: 元 shard と出力の全レコード数
- `replaced`: mask の bit 1 の数。compact の全レコード数と同じ
- `changed`: 書き戻し前後で score の値が実際に変わったレコード数
