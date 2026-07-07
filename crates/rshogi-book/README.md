# rshogi-book

`rshogi-book` は YANEURAOU-DB2016 テキスト `.db` 形式の定跡を読み込み、root 局面で
定跡手を 1 手 probe する crate です。USI 統合では `rshogi-usi` の Book 系オプションが
`BookOptions` に反映されます。

## 定跡 probe オプション

| USI オプション | 既定 | 説明 |
| --- | --- | --- |
| `USI_OwnBook` | `true` | 定跡使用の総合スイッチ。`false` なら probe しない。 |
| `BookFile` | `no_book` | 定跡ファイル名。`no_book` または空なら定跡をロードしない。 |
| `BookDir` | `book` | 相対 `BookFile` を解決するディレクトリ。 |
| `BookMoves` | `16` | この手数まで定跡を使う。 |
| `BookEvalDiff` | `30` | 候補中の最大 value からこの差分以内の手だけ残す。 |
| `BookEvalBlackLimit` | `0` | 先手番で採用する value 下限。 |
| `BookEvalWhiteLimit` | `-140` | 後手番で採用する value 下限。 |
| `BookDepthLimit` | `0` | 筆頭手の depth 下限。`0` は無効。 |
| `NarrowBook` | `false` | count 情報がある場合、出現率 10% 未満の手を除外する。 |
| `BookSelectValue` | `false` | 評価値フィルタ後の生存候補から value 最大手を決定的に選ぶ。同値は count 大、さらに同値なら USI 昇順。`true` のとき `NarrowBook` / `ConsiderBookMoveCount` / 等確率抽選より優先する。 |
| `ConsiderBookMoveCount` | `false` | `true` なら count 比例抽選する。全 count が 0 の場合は等確率。 |
| `IgnoreBookPly` | `false` | ロード時に SFEN の手数を無視してキーを正規化する。 |
| `FlippedBook` | `true` | miss 時に先後反転局面で再検索する。 |

選択順序は、合法性検証、`BookDepthLimit`、`BookEvalDiff` / value 下限フィルタの後に
`BookSelectValue` を評価します。`BookSelectValue=false` の場合は従来どおり
`NarrowBook` を適用し、その後 `ConsiderBookMoveCount` または等確率抽選で選びます。
