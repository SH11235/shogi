# relabel_psv — PSV 勝敗ラベルへの score 置換

`PackedSfenValue` の `score` を、各レコードの `game_result` から得た値へ置換する。
`game_result` と `score` はどちらもその局面の手番側視点で、勝ちを正、負けを負、引分を 0 とする。

処理は40バイトの PSV レコードを逐次読み書きする。複数入力や glob は辞書順へ固定して
処理するため、同じ入力とオプションからは同じ byte 列を得る。`--output` や
`--emit-verdict-sidecar` が入力 PSV、game_id sidecar、diversions result JSONL、または互いに同じ
ファイル実体を指す場合は、truncate を防ぐため処理開始前に拒否する。相対パスの別表記、symlink、
Unix 上の hardlink も検査する。

## 基本用法

```bash
cargo run -p tools --release --bin relabel_psv -- \
  --input "$SHOGI_DATA/teachers/input-*.psv" \
  --output "$SHOGI_DATA/teachers/relabelled.psv"
```

既定の `--win-cp 2500` では次のように上書きする。

| `game_result` | 新しい `score` |
|---:|---:|
| `1` | `+2500` |
| `0` | `0` |
| `-1` | `-2500` |

`--win-cp` は `1..32000` の範囲で指定する。score-drop の番兵値 32000 とは干渉しない。

## 宣言勝ち override

`--declaration-override` を指定すると各 PackedSfen を復号し、手番側が27点法で宣言勝ち可能な
局面の `score` を、対局結果にかかわらず `+win-cp` にする。復号と局面構築が必要なため既定は無効。

## diversions による deblunder

`gensfen --emit-game-id-sidecar` で作った sidecar と result JSONL を指定する。
result JSONL の diversion `ply` は開始局面からの相対手数であり、`start_sfen` の開始手数を使って
PSV の絶対 `game_ply` へ `start_ply + ply - 1` で変換する。PSV と sidecar は lockstep で読み、
件数不一致や末尾の半端レコードをエラーにする。

```bash
cargo run -p tools --release --bin relabel_psv -- \
  --input "$SHOGI_DATA/teachers/gensfen.psv" \
  --output "$SHOGI_DATA/teachers/gensfen-relabelled.psv" \
  --deblunder \
  --deblunder-mode drop-contaminated \
  --game-id-sidecar "$SHOGI_DATA/teachers/gensfen.game_ids.bin" \
  --diversions "$SHOGI_DATA/teachers/gensfen.jsonl"
```

| モード | 除外条件 | 既定 |
|---|---|---|
| `drop-before-last` | 最後の diversion 以前を除外する | yes |
| `drop-before-any` | 最初の diversion 以前を除外する | no |
| `drop-contaminated` | 汚染と判定した diversion のうち最大 ply 以前を除外する | no |

`--deblunder-mode` を明示する場合は `--deblunder` も必須である。モードだけを指定した実行は、
フィルタなしの出力を防ぐため開始前にエラーにする。

境界 ply のレコード自体も除外する。`drop-before-last` と `drop-before-any` は従来どおり
レコード単位の streaming 処理を行う。

### `drop-contaminated` の判定

判定に使う値は diversion ply の PSV レコードに保存された relabel 前の元 `score` (`e`)、
同じレコードの `game_result` (`r`)、result JSONL の `score_gap_cp` (`g`) である。`g` は
比較前に `-10000..=10000` へ clamp する。`--flip-threshold` の既定値は 300 cp、
`--gap-threshold` の既定値は 100 cp で、どちらも `0..=10000` の範囲を取る。

| 条件 | 判定 |
|---|---|
| `kind=random` | 汚染 |
| `r=±1`, `|e| >= flip-threshold`, `sign(e) == sign(r)` | 温存 |
| `r=±1`, `|e| >= flip-threshold`, `sign(e) != sign(r)` | flip 汚染 |
| `r=±1`, `|e| < flip-threshold` | `g < gap-threshold` なら温存、それ以外は gap 汚染 |
| `r=0` | `e` を使わず gap-only |
| diversion ply の PSV レコードが欠落 | gap-only。欠落だけを理由に汚染とはしない |

複数 diversion は個別に判定し、汚染 diversion の最大絶対 ply を除外境界にする。
汚染 diversion がなければその対局は全レコードを温存する。同じ最大 ply に複数の汚染理由が
ある場合、verdict 表示用の優先順位は `random > missing_record > flip > gap` とする。
`kind=multipv` の diversion に `score_gap_cp` がない場合は判定を続行できないため、エラーで停止する。

このモードだけは game_id が変わるまで1対局ぶんをバッファし、判定後に flush する。一度 flush した
game_id が再出現した場合や、PSV の game_id が result JSONL に存在しない場合はエラーにする。
したがって、game_id が非連続になる shuffle 済み PSV には適用できない。`drop-contaminated` は
shuffle 前に適用する。

ピークメモリは1対局の PSV レコード、result JSONL 由来の対局 map とその連続性フラグで決まる。
対局 map の基礎部分は対局数 × 約47バイトが目安で、10億局面を約1,000万対局とすると約470MBになる。
これに diversion 配列と allocator の管理領域が加わる。入力 PSV の総レコード数そのものには依存しない。

## dry-run と2段階運用

`--dry-run` は出力 PSV を作成せず、通常実行と同じ判定と統計集計を行う。このとき `--output` は
省略でき、指定しても作成しない。verdict sidecar は `--dry-run` と併用して出力できる。

```bash
# 1. drop 率と gap 分布を確認
relabel_psv ... --deblunder-mode drop-contaminated --dry-run

# 2. 閾値を合意した後に PSV を生成
relabel_psv ... --deblunder-mode drop-contaminated \
  --flip-threshold 300 --gap-threshold 100 --output relabelled.psv
```

## verdict sidecar

`--emit-verdict-sidecar PATH` は、入力 PSV の全レコードと 1:1・同順の u8 を1バイトずつ書く。
drop されないレコードも必ず1件書く。`--dry-run` と併用できる。

| 値 | 名前 | 意味 |
|---:|---|---|
| 0 | `kept` | 温存。diversion がない対局や汚染境界より後ろも含む |
| 1 | `dropped_flip` | 最大汚染境界の理由が score と result の符号不一致 |
| 2 | `dropped_gap` | 最大汚染境界の理由が gap 判定 |
| 3 | `dropped_missing_record` | diversion レコード欠落後の gap 判定 |
| 4 | `dropped_random` | 最大汚染境界の理由が `kind=random` |
| 5 | `dropped_legacy` | `drop-before-last` / `drop-before-any` による除外 |

## 統計 JSON

処理完了時に stderr へ JSON を1行出力する。

| フィールド | 内容 |
|---|---|
| `input_positions` | 入力局面数 |
| `wins` / `losses` / `draws` | game_result 別の relabel 件数 |
| `declaration_overrides` | 宣言勝ち override 件数 |
| `declaration_overrides_dropped` | override 対象かつ deblunder で除外された件数 |
| `deblunder_dropped_positions` | 除外局面数 |
| `diversion_games` | 入力 PSV に現れ、diversion を持つ対局数 |
| `contaminated_games` / `preserved_games` | 汚染あり / 汚染なしと判定した diversion 対局数 |
| `decisions.flip_contaminated` | flip 汚染 diversion 数 |
| `decisions.gap_contaminated` | レコードが存在する gap 汚染 diversion 数 |
| `decisions.missing_record_contaminated` | 欠落フォールバック後の汚染 diversion 数 |
| `decisions.missing_record_preserved` | 欠落フォールバック後の温存 diversion 数 |
| `decisions.random_contaminated` | random 汚染 diversion 数 |
| `draw_games_by_reason.max_moves` / `sennichite` / `other` | 入力 PSV に現れる `r=0` 対局の result `reason` 別件数 |

`gap_histogram` は、入力 PSV に1レコード以上現れた対局について、clamp 後の `g` を全 MultiPV
diversion から集計する。result JSONL にだけ存在し、入力 PSV に現れない対局は集計対象外である。
`boundaries_cp` は固定で `[0,50,100,200,300,500,1000,3000]`、`counts` は9要素で、順に
`g < 0`, `[0,50)`, `[50,100)`, `[100,200)`, `[200,300)`, `[300,500)`, `[500,1000)`,
`[1000,3000)`, `g >= 3000` を表す。`score_gap_cp=null` の random diversion は histogram に含めない。
`kind=random` は生成側で乱択前の保存局面を消去するため、汚染境界以前の PSV レコードが存在しない
ことがある。この場合、`contaminated_games > 0` でも `deblunder_dropped_positions == 0` になり得る。

## 判定の限界

- この判定は、乱択が結果を変えなかったことの因果的証明ではない。result とラベルの整合性フィルタである。
- 識別力は形勢決定的な局面に偏る。`|e| < flip-threshold` の互角局面は gap 判定だけに依存する。
- `g` は未クランプの生スコア差、`e` は PSV の `±10000` clip 値が入力であり、元のスケールが異なる。
- `gap-threshold` は生成 run の `--random-multi-pv-diff` と結合する。diff が gap-threshold 未満の run では gap 汚染分岐が発生しない。
- 既定値 flip=300 / gap=100 は仮置きである。本番 smoke の gap 分布と閾値ごとの温存率を確認して確定する。
