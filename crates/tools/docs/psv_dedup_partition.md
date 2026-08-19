# psv_dedup_partition

`psv_dedup_partition` は、大規模な PSV データから PackedSfen 重複を **完全一致 (exact)** で除去する低メモリ向けツールです。

- 入力: PackedSfenValue 40 バイト固定長ファイル
- 重複キー: 先頭 32 バイトの PackedSfen（`psv_dedup` と同じ方針）
- 出力: 最初に出現した局面のみを残した単一ファイル
- 方式: ディスクパーティショニング → パーティションごとに `HashSet<[u8; 32]>`
- 特徴: **偽陽性・偽陰性なし**、メモリは「最大パーティション分」に抑えられる
- 任意機能: `--shuffle-seed` で dedup と学習用 shuffle を同じ Phase 2 に統合

巨大データでも exact に dedup したいが、`psv_dedup`（全件 HashSet 保持）ではメモリが足りないケース向けです。

## 仕組み

### Phase 1: Partitioning

入力を順次ストリーミングで読み、PackedSfen (32 B) の 64 bit FNV-1a ハッシュを計算して `--partitions` 個の一時ファイルに振り分ける。

```
入力 .bin → hash_packed_sfen(sfen) % P → partition_NNNNN.bin
```

同じ局面は必ず同じパーティションに入るため、後段で「1 パーティション内で HashSet 判定」すれば全体の dedup と等価になる。

### Phase 2: Deduplication

各パーティションファイルを 1 つずつ読み込み、`HashSet<[u8; 32]>` で first-wins の重複判定を行い、出力ファイルに追記する。`--shuffle-seed` 指定時は unique レコードをパーティション単位で保持し、後述の shuffle 後に書き出す。処理済みパーティションの作業領域は次へ進む前に解放する。

`--keep-temp` を指定しない限り、処理済みパーティションは逐次削除される。

### fused shuffle (`--shuffle-seed`)

`--shuffle-seed <u64>` を指定すると、Phase 2 で first-wins の unique 選別を終えた後、書き出し直前にパーティション内のレコード列を Fisher-Yates で shuffle する。勝者選択後に順序だけを変えるため、seed の有無によって残るレコードの集合は変わらない。full mode と `--dedup-only` で利用でき、Phase 2 を実行しない `--partition-only` との同時指定はエラーになる。

乱数列は seed と partition index から SplitMix64 で状態を導出した xoshiro256++ で生成する。同じ順序の入力バイト列・partition 数・seed なら、プラットフォームをまたいで同じ出力バイト列を再現する。glob 展開や path ソートの結果が環境によって変わり、入力順が異なる場合はこの前提に含まれない。

PackedSfen の FNV hash による bucket 割当、bucket 内 shuffle、bucket 番号順の連結という、`shuffle_psv` の chunked shuffle と同様の二段構成（ランダム散布 + 区画内 shuffle + 連結）を取る。ただし fused shuffle は bucket 間を移動できないため順列分布は同一ではなく、学習用途への適合性は用途側で判断する必要がある。bucket の作り方と乱数列も異なるため、`shuffle_psv` と同じ seed を指定しても出力はバイト互換にならない。

### 一時ディレクトリ構造

```
<temp-dir>/
├── input/
│   ├── partition_00000.bin
│   ├── partition_00001.bin
│   └── ...
└── ref/              # --reference 指定時のみ
    ├── partition_00000.bin
    └── ...
```

## reference モード

既存の dedup 済みファイルがあり、新規データから「その既存ファイルに含まれない局面」だけを抽出したい場合に `--reference` を使う。参照ファイルは出力に含まれず、HashSet 登録のみ行われる。

Phase 2 の流れ (パーティションごと):

1. `ref/partition_{i}.bin` を HashSet に全件ロード（出力しない）
2. `input/partition_{i}.bin` を streaming し、HashSet に未登録なら選別 + 挿入
3. HashSet を解放して次パーティションへ

これにより、既存ファイルと新規ファイルを結合してから dedup するよりも I/O が少なく、かつ reference 側の重複は出力に回らない。`psv_dedup_bloom --reference` の完全一致版に相当。

`--dedup-only` で再開する際は `<temp-dir>/ref/` の存在を自動検出し、reference モードとして処理する（`--reference` 再指定は不要）。

## 事前見積りと不足チェック

起動時に Phase 1 / Phase 2 のメモリとディスク要件を見積もり、不足時は実行前に停止する。

```
=== Resource Estimate ===
Total input records:  17000000000 (680000000000 bytes / 40)
Phase 1 temp disk:    633.2 GiB (cleaned up on success)
Phase 1 memory:       0.5 GiB (fixed: partitions × buffer)
Phase 2 peak memory:  1.3 GiB (HashSet of largest partition, ~1.20x variance)
Memory available:     64.0 GiB (threshold 80% = 51.2 GiB)
Temp disk available:  8.0 TiB (/fast/ssd/tmp)
Output disk:          same filesystem as temp (/fast/ssd). 追加 headroom ...
```

チェック内容:

- **メモリ**: `Phase1 mem` と `Phase2 peak mem` のうち大きい方が OS の利用可能メモリ × 80% を超えたら Err。利用可能メモリを取得できない場合も `--force` なしでは停止
- **temp ディスク**: 入力合計 × 1.05 が `--temp-dir` の空きを超えたら Err
- **output ディスク**: 出力上限 = 入力合計（dedup しない最悪ケース）× 1.05 が出力親ディレクトリの空きを超えたら Err。temp と同一ファイルシステムの場合もチェックは省略しない。`--keep-temp` なしでは「処理中の最大 input partition」ぶんの追加 headroom、`--keep-temp` ありでは output 全量ぶんの追加空き容量を要求する
- 不足時は停止。`--force` を付けると Warning を出して続行する（swap 多用・途中失敗のリスクを許容する場合のみ）

## 一時ディスクの寿命

- **Phase 1 実行中〜Phase 2 開始時点**: peak 使用量 ≈ 入力合計サイズ
- **Phase 2 実行中**: partition ごとに処理→削除するので、temp 使用量は徐々に減る
- **完走時**: 一時ファイルと一時ディレクトリを全て削除（`--keep-temp` 時のみ保持）
- **出力ファイル**: 別途 `--output` に書き出され、完走後も残る

## メモリ見積り

Phase 2 のピークメモリはおおよそ次の式で決まる:

```
peak_memory ≈ (total_records / partitions) × entry_overhead
```

`entry_overhead` は `HashSet<[u8; 32]>` で 1 エントリあたり 50〜70 B 程度（バケット + エントリ + load factor）。
`--shuffle-seed` 指定時は、これに最大 input partition のレコード列（1 件 40 B）が加わる。起動時の事前見積りもこの shuffle buffer を含める。

### 参考値（均等分布を仮定）

| 総レコード数 | `--partitions 1024` | `--partitions 4096` |
|---:|---:|---:|
| 10 億      | ~60 MiB       | ~15 MiB       |
| 100 億     | ~600 MiB      | ~150 MiB      |
| 250 億     | ~1.5 GiB      | ~370 MiB      |

Phase 1 の固定コストは `partitions × partition_buffer_kb`。デフォルト (`1024 × 64 KiB = 64 MiB`) で十分で、メモリが更に厳しい場合は `--partition-buffer-kb 16` 等に縮小できる。

## 一時ディスク

Phase 1 は入力と **ほぼ同サイズ** のデータを一時ディレクトリに書き出す。`--temp-dir` は入力と同等以上の空き容量がある場所を指定すること（デフォルト: `./psv_dedup_partition_tmp`）。

Phase 2 の進行に合わせて一時ファイルは削除されるので、ピーク使用量は入力サイズ程度。`--keep-temp` 指定時は残る。

## I/O

合計 I/O は通常 dedup の約 2 倍（Phase 1 で write once、Phase 2 で read once）。一方 `psv_dedup_bloom` は 1 パスなので、「メモリ vs ディスク I/O」のトレードオフになる。

`--shuffle-seed` は Phase 2 の書き出し前に shuffle を済ませるため、後段の `shuffle_psv` が必要とする 2 パスの chunked shuffle（read × 2 + write × 2）を工程ごと省略できる。fused shuffle 自体は dedup に追加のディスクパスを増やさない。

## FD 上限

`--partitions 1024` 指定時は ulimit の soft limit が 1024 以上必要。起動時にチェックして不足なら警告する。Linux のデフォルトは 1024 なので、多くの環境で次の指定が必要:

```bash
ulimit -n 4096
```

## 使用例

### 基本: ディレクトリ内の全ファイルを exact dedup

```bash
cargo run --release -p tools --bin psv_dedup_partition -- \
  --input-dir ../bullet-shogi/data/DLSuisho15b \
  --pattern "*.bin" \
  --output /path/to/deduped.bin \
  --temp-dir /fast/ssd/psv_tmp
```

### dedup と学習用 shuffle を同時に行う

```bash
cargo run --release -p tools --bin psv_dedup_partition -- \
  --input-dir /path/to/dir \
  --output deduped_shuffled.bin \
  --temp-dir /fast/ssd/psv_tmp \
  --shuffle-seed 42
```

既存 temp から再開する場合も `--dedup-only --shuffle-seed 42` のように指定できる。

### メモリをさらに絞る

```bash
cargo run --release -p tools --bin psv_dedup_partition -- \
  --input-dir /path/to/dir \
  --output deduped.bin \
  --temp-dir /fast/ssd/psv_tmp \
  --partitions 4096 \
  --partition-buffer-kb 16
```

### 既存 dedup 済みファイルとの差分だけ抽出 (reference モード)

```bash
cargo run --release -p tools --bin psv_dedup_partition -- \
  --reference existing_deduped.bin \
  --input new_data.bin \
  --output unique_new.bin \
  --temp-dir /fast/ssd/psv_tmp
```

複数の reference を指定する場合はカンマ区切り:

```bash
  --reference old1.bin,old2.bin,old3.bin \
```

### Phase 1 だけ先に済ませて後日 Phase 2

`--keep-temp` で一時ファイルを保持し、別セッションで `--dedup-only` から再開できる。途中で partition ファイルが欠けた temp ディレクトリは破損扱いとなり、`--dedup-only` は即エラーで停止する。

```bash
# セッション1: 振り分けだけ
cargo run --release -p tools --bin psv_dedup_partition -- \
  --input-dir /path/to/dir \
  --output deduped.bin \
  --temp-dir ./psv_tmp \
  --keep-temp

# セッション2: 後日 Phase 2 のみ
cargo run --release -p tools --bin psv_dedup_partition -- \
  --output deduped.bin \
  --temp-dir ./psv_tmp \
  --dedup-only
```

### 数 TB クラス: 入力サイズの 2 倍の空きが取れない場合

通常モードは Phase 1 完了時点で「入力ぶんの一時ファイル + 元入力」が同時に存在するため、入力と同等以上の空きが必要になる。`--partition-only` はこの制約を緩めるためのモードで、Phase 1 (振り分け) だけを実行し、入力ファイルを 1 つずつ処理→削除して進められる。

```bash
# glob は shell ではなくツール側で展開するため、クォートして渡す。
# 各入力ファイルは、読み切りと partition 書き込みの flush が成功した後に削除される。
cargo run --release -p tools --bin psv_dedup_partition -- \
  --partition-only \
  --input "/data/psv/*.bin" \
  --temp-dir /fast/ssd/psv_tmp \
  --partitions 1024 \
  --delete-input-on-success

# 全件の振り分けが終わったら最後に Phase 2 だけ実行
cargo run --release -p tools --bin psv_dedup_partition -- \
  --dedup-only \
  --output deduped.bin \
  --temp-dir /fast/ssd/psv_tmp
```

挙動と制約:

- 既存の `partition_NNNNN.bin` には append モードで追記される
- `--input` にはファイル、ディレクトリ、glob を指定できる。複数指定はカンマ区切り
- glob はツール側で展開する。Unix shell で先に展開されないよう `"/data/psv/*.bin"` のようにクォートする
- `--delete-input-on-success` は `--partition-only` 専用。各入力ファイルを完全に読み切り、partition 書き込みの flush に成功した場合だけ元ファイルを削除する
- `--delete-input-on-success` と `--max-positions` は併用不可
- 2 回目以降の `--partitions` が初回と異なるとエラー (ハッシュ空間不整合を防止)
- `--reference` は **初回のみ** 指定可能。`ref/` に reference データが書き込まれた状態で再指定するとエラー
  - 過去の中途失敗で `ref/` に空 partition ファイルだけが残っている場合は失敗の残骸とみなしてリトライを許容する (空ファイルへの append は新規作成と等価)
  - 部分書き込みが残っている場合は手動で `ref/` を削除してから再実行する (append 復旧は二重登録になるため自動化していない)
- `--keep-temp` は暗黙で有効、`--output` は指定不可
- 出力ディスクの事前チェックはスキップ。temp ディスクは「今回追加するぶん」だけ見積もる

## オプション一覧

| オプション | 説明 | デフォルト |
|---|---|---|
| `--reference` | 参照ファイル。`--dedup-only` は指定値を使わず `ref/` を自動検出 | — |
| `--input` | 入力ファイル、ディレクトリ、glob（カンマ区切り）。`--input-dir` と排他 | — |
| `--input-dir` | 入力ディレクトリ。`--pattern` と組み合わせ | — |
| `--pattern` | `--input-dir` 使用時の glob パターン | `*.bin` |
| `--output` | 出力ファイルパス。full / `--dedup-only` では必須、`--partition-only` では指定不可 | — |
| `--temp-dir` | パーティション一時ファイルの置き場 | `./psv_dedup_partition_tmp` |
| `--partitions` | パーティション数（1 以上）。`--dedup-only` は既存 temp から自動検出 | `1024` |
| `--partition-buffer-kb` | 各パーティションの BufWriter バッファ (KiB、1 以上) | `64` |
| `--max-positions` | Phase 1 で処理する入力レコード上限（0 = 全件）。参照は常に全件 | `0` |
| `--dedup-only` | 既存一時ファイルから Phase 2 を再開。`--input` / `--input-dir` / `--partition-only` と併用不可 | off |
| `--partition-only` | Phase 1 のみ実行（既存 partition へ append）。`--dedup-only` と併用不可 | off |
| `--delete-input-on-success` | `--partition-only` で各入力ファイルの処理成功後に元ファイルを削除する | off |
| `--keep-temp` | 完了後も一時ファイル・ディレクトリを削除しない（`--partition-only` では暗黙で有効） | off |
| `--shuffle-seed` | Phase 2 の unique レコードをパーティション内 shuffle する seed。`--partition-only` と併用不可 | — |
| `--force` | メモリ/ディスク不足でも警告のみで続行する | off |

## `psv_dedup` / `psv_dedup_bloom` との比較

| 項目 | `psv_dedup` | `psv_dedup_bloom` | `psv_dedup_partition` |
|---|---|---|---|
| 方式 | 全件 `HashSet<u64>` | Blocked Bloom Filter | ディスクパーティション + `HashSet<[u8;32]>` |
| 正確性 | ほぼ exact (64bit hash 衝突のみ) | 近似 (`--fpr` で制御、偽陽性あり) | **完全 exact** |
| メモリ | ユニーク局面数 × 約 16 B | 固定 (入力規模と `fpr` から決定) | 最大パーティションのユニーク局面ぶん |
| 一時ディスク | 不要 | 不要 | 入力と同等 |
| I/O パス数 | 1 パス | 1 パス | 2 パス (write + read) |
| 向いている規模 | 数億〜数十億 | 数百億 (メモリ潤沢) | 数十億〜数百億 (メモリ限定) |

選び方は [pack_tools.md の「重複除去ツールの選び方」](pack_tools.md#重複除去ツールの選び方) を参照。

## 注意

- 入力と出力が同一ファイルの場合はエラー
- キーは PackedSfen のみ。同一局面に対する複数の教師手がある場合は `psv_dedup` と同様、最初の出現だけ残す
- seed 未指定でもパーティション番号順に連結するため、入力全体の順序は保存されない（従来どおり）。`--shuffle-seed` 指定時はパーティション内の順序も shuffle される
