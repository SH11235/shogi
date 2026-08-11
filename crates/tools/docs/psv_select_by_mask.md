# psv_select_by_mask — bitmap mask による PSV 行抽出

教師 PSV shard から mask の bit 1 に対応する行だけを入力順のまま抽出し、`rescore_psv`
などへ渡す compact PSV を作ります。40 byte レコードは解釈・変更せず、そのままコピーします。
入力全体をメモリへ載せないチャンクストリーミング処理です。

## 使い方

```bash
cargo run --release -p tools --bin psv_select_by_mask -- \
  --input shard.psv \
  --mask entered.bits \
  --out compact.psv
```

正常終了時は次の形式で件数を標準出力へ表示します。

```text
records=1000000 selected=12345 (1.2345%)
```

## mask 契約

- LSB-first bitmap。byte `j` の bit `k` は record `j * 8 + k` に対応する
- bit 1 の行だけを抽出し、bit 0 の行は出力しない
- サイズは `ceil(records / 8)` byte と厳密に一致する
- 最終 byte でレコードに対応しない未使用 bit はすべて 0 とする

入力 PSV のサイズが 40 byte の倍数でない場合、mask のサイズが一致しない場合、または
未使用 bit が 1 の場合は出力を確定せずに失敗します。出力は `<out>.partial` へ書き、抽出件数から
求めた期待サイズとの一致を検査してから `--out` へ rename します。

## compact PSV の書き戻し対応

再ラベル後の compact PSV を元の shard に対応付けるときは、`shard + mask + compact 列` を
先頭から同時に walk します。mask の bit 1 ごとに compact 側を 1 行進め、その行を同じ shard
行へ対応させます。抽出順は shard 内の行順から変わらないため、追加の行 ID や並べ替えは不要です。
