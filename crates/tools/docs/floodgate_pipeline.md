# floodgate_pipeline

Floodgate（`wdoor.c.u-tokyo.ac.jp`）の公開棋譜を取得し、NNUE 学習向けに
CSA → SFEN へ変換するパイプライン。サブコマンドは `fetch-ratings` /
`fetch-index` / `download` / `live-mirror` / `extract` の 5 つ。

通信は `reqwest`（rustls）で行う。Floodgate は http アクセスを https へ 301
誘導するため、既定 root は https を直接指す（`--root` に http を渡しても
サーバ側リダイレクトで動作する）。

## サブコマンド

### `fetch-ratings`

レーティングページから高レートプレイヤー名を取得し、`download` の事前フィルタ
（`--player-file`）に使うリストを出力する。

```bash
cargo run -p tools --bin floodgate_pipeline -- fetch-ratings --min-rating 3900 --out high_rated.txt
```

- `--url <URL>`: レーティングページ URL。**未指定なら直近の日付のページを自動取得**する。
  ページは日次生成の日付スタンプ付き（`.../shogi/x/rating/players-floodgate-YYYYMMDD.html`）で、
  サーバ（JST）基準の当日から最大 7 日遡って最初に取得できたものを採用する。
- `--min-rating <N>`: この値以上のプレイヤーを出力（既定 3900）。
- `--out <PATH>`: 出力先。1 行 1 プレイヤーで `名前<TAB>レート` 形式。

### `fetch-index`

棋譜インデックス `00LIST.floodgate` をダウンロードする。

```bash
cargo run -p tools --bin floodgate_pipeline -- fetch-index --out 00LIST.floodgate
```

- `--root <URL>`: Floodgate root（既定は https）。
- `--out <PATH>`: 出力先。

### `download`

インデックスに記載された CSA ファイルを並列ダウンロードする。インデックス内の
絶対パスは末尾の `/x/` 以降を相対パスとして `--out-dir` 配下へ配置する。

```bash
cargo run -p tools --bin floodgate_pipeline -- download \
  --index 00LIST.floodgate --date-from 2026-03-10 --player-file high_rated.txt --concurrency 16
```

- `--index <PATH>` / `--root <URL>` / `--out-dir <DIR>`。
- `--limit <N>`: ダウンロード数の上限（テスト用）。
- `--date-from` / `--date-to <YYYY-MM-DD>`: 日付フィルタ。
- `--player-file <PATH>`: いずれかの対局者が含まれる対局のみ取得（`fetch-ratings` の出力を渡す）。
- `--concurrency <N>`: 並列数（既定 8、`0` で CPU コア数）。

### `live-mirror`

当日 (JST) の対局 CSA をローカル dir へミラーし続ける。`kifu_player --csa <dir> --live`
と組み合わせて、wdoor floodgate のほぼリアルタイム観戦に使う。両者を 1 コマンドに
まとめた wrapper 例が [docs/examples/floodgate/](../../../docs/examples/floodgate/README.md)
にある（`watch.sh` / `watch.ps1`）。

```bash
cargo run -p tools --bin floodgate_pipeline -- live-mirror \
  --out-dir /tmp/wdoor-live --watch RAMU_TF,Suisho --interval 10
```

- `--out-dir <DIR>`: ミラー先（ファイル名は wdoor のまま。`kifu_player` の日付ソート・
  `date:` フィルタが末尾 14 桁タイムスタンプを認識する）。
- `--interval <SECS>`: 進行中対局の再取得間隔（既定 10 秒）。対局一覧（日次 autoindex）
  の確認は約 60 秒ごと（`--interval` がそれより長い場合はそのパス頻度。ペアリングは
  毎時 :00/:30 のため秒単位で追う意味は薄い）。
- `--watch <NAMES>`: 対局者名の部分一致フィルタ（カンマ区切り）。未指定は当日の全対局。
- `--once`: 1 パスだけ実行して終了（動作確認用）。

挙動:

- wdoor の CSA は対局開始時に作られ**進行中も逐次追記**されるため、間隔ごとに毎回取得する
  （`Last-Modified` は秒精度で同秒追記を 304 に取り逃がすため条件付き GET は使わない。
  本文は数十 KB で転送コストは小さい）。追記のみで単調増加する性質から、同サイズなら
  ローカルへの書き込みを省く。
- **終局（`'$END_TIME:` コメント）を検出したファイルは以後取得しない**。過去の実行で
  ミラー済みの完全な棋譜も再取得しない。約 1 時間変化の無い進行中ファイルは中断対局と
  みなし追跡を止める（部分棋譜は「勝敗不明」として残る）。
- 書き込みは tmp→rename の全置換なので、`kifu_player --live` 側は常に完全な内容だけを読む。
- 取得は逐次 (1 リクエスト 30 秒タイムアウト)。`--watch` 無しで進行中対局が非常に多い日は
  1 パスが `--interval` を超えることがある (その場合は間隔を長めに、または `--watch` で絞る)。
- 停止は Ctrl-C。

### `extract`

ローカルの CSA から学習用 SFEN を抽出する。

```bash
cargo run -p tools --bin floodgate_pipeline -- extract --min-rating 3900 --max-ply 32
```

- `--root <DIR>` / `--out <PATH>`（`-` で標準出力、`.gz` 対応）。
- `--mode <initial|all|nth>`、`--nth <手数,...>`、`--min-ply` / `--max-ply`。
- `--mirror-dedup`（水平ミラー正規化で重複排除）/ `--emit-mirror`。
- `--per-game-cap <N>`（1 棋譜あたりの最大抽出数）/ `--min-rating <N>`。

## 典型的な流れ

```bash
# 1. 高レートプレイヤーリスト
cargo run -p tools --bin floodgate_pipeline -- fetch-ratings --min-rating 3900 --out high_rated.txt
# 2. インデックス取得
cargo run -p tools --bin floodgate_pipeline -- fetch-index --out 00LIST.floodgate
# 3. CSA ダウンロード（日付・プレイヤーでフィルタ）
cargo run -p tools --bin floodgate_pipeline -- download --date-from 2026-03-10 --player-file high_rated.txt --concurrency 16
# 4. SFEN 抽出
cargo run -p tools --bin floodgate_pipeline -- extract --min-rating 3900 --max-ply 32
```
