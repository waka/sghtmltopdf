# CSSプロパティ対応表

- [表示・可視性](#表示可視性)
- [ボックスモデル](#ボックスモデル)
- [枠線・角丸・アウトライン・影](#枠線角丸アウトライン影)
- [配置(positioning / float / transform)](#配置positioning--float--transform)
- [フォント・テキスト](#フォントテキスト)
- [背景](#背景)
- [テーブル](#テーブル)
- [リスト](#リスト)
- [生成コンテンツ・カウンタ](#生成コンテンツカウンタ)
- [ページ分割(CSS Fragmentation)](#ページ分割css-fragmentation)
- [Flexbox](#flexbox)
- [Grid](#grid)
- [置換要素(画像)](#置換要素画像)
- [非対応プロパティ一覧](#非対応プロパティ一覧)

## 表示・可視性

| プロパティ | 対応 | 備考 |
| - | - | - |
| `display` | ⚠️ | `block`/`inline`/`inline-block`/`list-item`/`table`/`table-row`/`table-cell`/`table-caption`/`flex`/`none`のみ。`grid`は対応([Grid](#grid)節)。`inline-flex`/`inline-grid`・`table-row-group`等のテーブル内部値・`flow-root`は非対応。`<thead>`/`<tbody>`/`<tfoot>`はUAスタイルで`block`のままだが、行収集が透過するのでテーブルとして機能する |
| `visibility` | ⚠️ | `visible`/`hidden`/`collapse`。`collapse`は`hidden`と同一視(テーブル行/列の高さ再計算はしない)。継承プロパティ |
| `overflow` | ⚠️ | `visible`以外(`hidden`/`scroll`/`auto`)は区別せず一律クリップ。スクロールバーの概念は無い。`overflow-x`/`overflow-y`は非対応 |
| `opacity` | ⚠️ | `<number>`/`<percentage>`を0〜1にクランプ。PDFの透明グループ+ExtGStateで実装。要素単位の合成で、`mix-blend-mode`等のブレンドは非対応 |
| `z-index` | ⚠️ | `auto`/`<integer>`。`position: relative`の要素にのみ効く(仕様では他のpositioned要素にも効く)。同じ親を持つ兄弟間の描画順のみを制御し、スタッキングコンテキストの分離は非対応。絶対配置要素は常に通常フローの上に描かれる |
| `box-sizing` | ⚠️ | `content-box`/`border-box`。標準外の`padding-box`は非対応 |

## ボックスモデル

| プロパティ | 対応 | 備考 |
| - | - | - |
| `width` / `height` | ⚠️ | `auto`/`<length>`/`<percentage>`/`calc()`。`min-content`/`max-content`/`fit-content`は非対応。`height`のパーセンテージはcontaining block高さ不定として無視される |
| `min-width` / `min-height` | ⚠️ | `<length>`/`<percentage>`/`calc()`(初期値`0`)。`auto`/`min-content`等のキーワードは非対応。`min-height`のパーセンテージは無視される |
| `max-width` / `max-height` | ⚠️ | `none`/`<length>`/`<percentage>`/`calc()`(初期値`none`)。`min > max`のときは`min`が勝つ(仕様通り)。`max-height`のパーセンテージは無視される |
| `aspect-ratio` | ⚠️ | `auto \| <ratio> \| auto <ratio>`。「幅確定→高さ導出」が基本で、「高さ確定→幅導出」はfloat/`inline-block`/絶対配置/`<img>`のshrink-to-fit文脈のみ(通常フローのブロックの`width: auto`はstretch優先、仕様通り)。`min-*`/`max-*`でクランプされて比が崩れた場合の再計算は行わない |
| `margin` | ✅ | 1〜4値ショートハンド。`auto`(中央寄せ)・負値に対応 |
| `margin-top` / `-right` / `-bottom` / `-left` | ✅ | 隣接兄弟間・親子間のマージン相殺に対応 |
| `padding` | ✅ | 1〜4値ショートハンド |
| `padding-top` / `-right` / `-bottom` / `-left` | ✅ | パーセンテージはcontaining block幅基準(仕様通り) |
| `margin-inline` / `margin-block` / `margin-inline-start`等 | ⚠️ | 論理プロパティ。対応する書字方向が`horizontal-tb`+LTRのみなので、`inline-start`=左、`inline-end`=右、`block-start`=上、`block-end`=下へ固定で写像する(`writing-mode`/`direction`は非対応)。2辺ショートハンドは1〜2値(`start end`の順)。`auto`も通るので`margin-inline: auto`で中央寄せできる |
| `padding-inline` / `padding-block` / `padding-inline-start`等 | ⚠️ | 論理プロパティ。写像規則は`margin-inline`と同じ |

## 枠線・角丸・アウトライン・影

| プロパティ | 対応 | 備考 |
| - | - | - |
| `border` | ✅ | `<width> \|\| <style> \|\| <color>`を任意順・任意省略で受け付け、4辺へ適用 |
| `border-top` / `-right` / `-bottom` / `-left` | ✅ | 辺別ショートハンド(値文法は`border`と同じ) |
| `border-inline` / `border-block` / `border-inline-start`等(`-width`/`-style`/`-color`のロングハンドを含む) | ⚠️ | 論理プロパティ。写像規則は[`margin-inline`](#ボックスモデル)と同じ |
| `border-width` / `border-style` / `border-color` | ✅ | 1〜4値ショートハンド |
| `border-*-width` | ⚠️ | `<length>`のみ。`thin`/`medium`/`thick`キーワードは非対応 |
| `border-*-style` | ⚠️ | `none`/`hidden`/`solid`/`dashed`/`dotted`/`double`/`groove`/`ridge`/`inset`/`outset`。`hidden`は`none`と同一視(テーブルの枠線競合解決でも区別しない)。`groove`/`ridge`/`inset`/`outset`は`border-color`から2階調の陰影を算出して描画 |
| `border-*-color` | ✅ | 初期値は`currentcolor` |
| `border-radius` | ⚠️ | 1〜4値+`/`区切りの楕円構文に対応。パーセンテージ指定は非対応(`<length>`のみ)。4辺の太さ・スタイル・色が揃っていない場合は角丸を諦めて直線4辺へフォールバックする。`groove`/`ridge`/`inset`/`outset`との併用も同様にフォールバック |
| `border-top-left-radius`ほか3隅 | ⚠️ | `<length>{1,2}`(水平/垂直半径)。制約は`border-radius`と同じ |
| `border-start-start-radius`ほか3隅 | ⚠️ | 論理プロパティ。1つ目がblock方向、2つ目がinline方向を指す(`border-start-end-radius`=右上)。写像規則は[`margin-inline`](#ボックスモデル)と同じ |
| `outline` | ✅ | `<width> \|\| <style> \|\| <color>`。border-boxの外側に描画し、レイアウトには影響しない |
| `outline-width` / `outline-style` / `outline-color` | ⚠️ | `outline-style`は`border-style`と同じ値集合。UA依存の`auto`は非対応 |
| `outline-offset` | ❌ | 常に0固定 |
| `box-shadow` | ⚠️ | `none \| <shadow>#`(カンマ区切りで複数指定可、先頭が最前面)。`inset`はパースするが描画は非対応。ぼかしは4段階の同心矩形による近似 |

## 配置(positioning / float / transform)

| プロパティ | 対応 | 備考 |
| - | - | - |
| `float` | ✅ | `none`/`left`/`right`。`width: auto`のshrink-to-fitに対応 |
| `clear` | ✅ | `none`/`left`/`right`/`both` |
| `position` | ⚠️ | `static`/`relative`/`absolute`/`fixed`。`sticky`は非対応。`absolute`/`fixed`には後述の制約あり |
| `top` / `right` / `bottom` / `left` | ⚠️ | `absolute`/`fixed`では`bottom`単独指定による下端揃えが非対応(高さの循環参照を避けるため`top`基準に解決する)。`relative`では背景・枠線と中身(テキスト・画像・子ボックス)をまとめてずらすオフセットとして機能する。`relative`の`top`/`bottom`のパーセンテージは0扱い |
| `inset`(ショートハンド) | ✅ | 1〜4値ショートハンド(展開規則は`margin`と同じ) |
| `inset-inline` / `inset-block` / `inset-inline-start`等 | ⚠️ | 論理プロパティ。写像規則は[`margin-inline`](#ボックスモデル)と同じ |
| `transform` | ⚠️ | `translate`/`translateX`/`translateY`/`scale`/`scaleX`/`scaleY`/`rotate`/`skew`/`skewX`/`skewY`/`matrix`。3D系(`translate3d`/`rotate3d`/`perspective()`等)は非対応。PDFのCTM変換で実装するため、変換後の内容はページ分割の判定に影響しない |
| `transform-origin` | ⚠️ | `background-position`と同じ値文法(キーワード/長さ/パーセンテージの1〜2値)。初期値`50% 50%`。3値目(Z軸)は非対応 |

`position: absolute`/`fixed`の既知の制約:

* 絶対配置要素は「通常フローから外し、確定したページへ後付けするオーバーレイ」として配置する
* containing blockになれるのは、単一ページに収まっているpositioned祖先(またはページ領域)
* テキストの途中に置いた`absolute`は、その前後のテキストが別々の行に分かれる(ブラウザのように1行のまま残らない)
* 絶対配置要素自身がページを跨ぐ分割は非対応(1ページにbest-effortで置く)
* ストリーミングモード(`Mode::Streaming`)では絶対配置を無視する

## フォント・テキスト

| プロパティ | 対応 | 備考 |
| - | - | - |
| `font-family` | ⚠️ | カンマ区切りリスト。汎用family名は`serif`/`sans-serif`/`monospace`をシステムフォントから解決する(`cursive`/`fantasy`は解決しない)。`sans-serif`の解決先はCLIの`--gothic-font`で決定的に上書きできる |
| `font-size` | ⚠️ | `<length>`(`px`/`em`/`rem`)のみ。`smaller`/`larger`/`medium`等のキーワード、パーセンテージ指定は非対応 |
| `font-weight` | ⚠️ | `normal`/`bold`/`100`〜`900`。数値は600以上を`bold`とみなす2値化。太字フォントが無い場合は塗り+縁取りの疑似ボールドで描画 |
| `font-style` | ⚠️ | `normal`/`italic`/`oblique`(`oblique`は`italic`と同一視、傾斜角の指定は不可)。イタリック字形が無い場合はテキスト行列のせん断による疑似イタリック |
| `font`(ショートハンド) | ❌ | 個別のロングハンドを使う |
| `color` | ✅ | 継承プロパティ。指定できる色の記法は[セレクタ・値・at-rule](selectors.md#色)を参照 |
| `line-height` | ✅ | `normal`/`<number>`/`<length>`/`<percentage>`。`normal`はフォント自身の推奨行送り(アセント+ディセント+行間)から求めるため、フォントによって値が変わる |
| `text-align` | ⚠️ | `left`/`right`/`center`/`justify`。`justify`は最終行以外の単語間で余白を配分する。`start`/`end`は非対応(`direction`自体が非対応のため) |
| `text-indent` | ⚠️ | `<length>`/`<percentage>`。`hanging`/`each-line`は非対応 |
| `text-transform` | ⚠️ | `none`/`uppercase`/`lowercase`/`capitalize`(語頭のみ変換)。`full-width`/`full-size-kana`は非対応 |
| `text-decoration` / `text-decoration-line` | ⚠️ | `none`/`underline`/`line-through`(併記可)。`overline`/`blink`は非対応。ショートハンドの`text-decoration-color`/`-style`/`-thickness`部分も非対応。祖先から子孫への伝播は「継承プロパティとして扱う」簡略実装 |
| `text-shadow` | ⚠️ | `none \| <shadow>#`(`<offset-x> <offset-y> <blur>? <color>?`)。PDFにぼかしフィルタが無いため、blurはアルファを下げた多重描画による近似。継承プロパティ |
| `text-overflow` | ⚠️ | `clip`/`ellipsis`。`overflow`が`visible`以外のときのみ有効。幅方向にはみ出した行のみが対象(ブロック全体のオーバーフローは扱わない)。`<string>`指定は非対応 |
| `word-break` | ✅ | `normal`(CJK文字が隣接する境界のみ改行可)/`break-all`/`keep-all`。非推奨値`break-word`は非対応 |
| `overflow-wrap` / `word-wrap` | ⚠️ | `normal`/`break-word`/`anywhere`(`anywhere`は`break-word`と同一視)。改行機会は増やさず、行頭に置いても収まらない語だけを文字単位で割る |
| `hyphens` | ⚠️ | `none`/`manual`/`auto`。soft hyphen(U+00AD)でのみ分割し、分割時に行末へハイフンを表示する。`auto`は辞書を持たないため`manual`と同じ挙動(自動ハイフネーションはしない) |
| `text-emphasis` / `-style` / `-color` / `-position` | ⚠️ | `dot`/`circle`/`double-circle`/`triangle`/`sesame`(`filled`/`open`)と`<string>`。キーワードのマークはPDFのパスで描くためフォントの字形に依存しない(`<string>`はグリフ描画で、字形が無ければ描かれない)。`position`は`over`/`under`のみ(`right`/`left`は読み飛ばし)。マーク分だけ行の高さが広がる。句読点をスキップする`text-emphasis-skip`は非対応 |
| `letter-spacing` | ⚠️ | `normal`/`<length>`。パーセンテージは非対応 |
| `word-spacing` | ⚠️ | `normal`/`<length>` |
| `white-space` | ⚠️ | `normal`/`nowrap`/`pre`。`pre-wrap`/`pre-line`/`break-spaces`は非対応 |
| `vertical-align` | ✅ | `baseline`/`sub`/`super`/`text-top`/`text-bottom`/`top`/`middle`/`bottom`/`<length>`/`<percentage>`。テーブルセル文脈では`top`/`middle`/`bottom`(と`baseline`)が意味を持つ |
| `quotes` | ⚠️ | `none`または`"開き" "閉じ"`のペアの繰り返し。`content`の`open-quote`/`close-quote`と組で使う。継承プロパティ |

## 背景

| プロパティ | 対応 | 備考 |
| - | - | - |
| `background`(ショートハンド) | ⚠️ | `<color>`/`<image>`/`<repeat>`/`<attachment>`/`<position>[ / <size>]`を任意順で受け付ける。指定しなかったロングハンドは仕様通り初期値へリセットされる。`background-clip`/`-origin`(`padding-box`等のキーワード)を含むとパースエラーになり宣言ごと無視される点に注意 |
| `background-color` | ✅ | アルファ付きの色はExtGStateで透過描画 |
| `background-image` | ⚠️ | `none \| url(...)`のみ。`linear-gradient()`等のグラデーション関数、カンマ区切りの複数背景は非対応。既定ではintrinsicサイズでタイル配置 |
| `background-position` | ✅ | キーワード(`left`/`center`/`right`/`top`/`bottom`)と長さ/パーセンテージの1〜2値。3〜4値構文(`right 10px bottom 20px`)は非対応 |
| `background-size` | ✅ | `cover`/`contain`/`<length-percentage> \| auto`の1〜2値 |
| `background-repeat` | ⚠️ | `repeat`/`repeat-x`/`repeat-y`/`no-repeat`。CSS3の`round`/`space`、2値構文は非対応 |
| `background-attachment` | ⚠️ | `scroll`/`fixed`(スクロールの概念が無いため`fixed`は`scroll`と同一視) |
| `background-clip` / `background-origin` / `background-blend-mode` | ❌ | 未実装。背景はborder-box基準で描画する |

`border-radius`と`background-image`を併用した場合、角丸によるクリップは行わない(角丸は背景色の塗りにのみ効く)。

## テーブル

| プロパティ | 対応 | 備考 |
| - | - | - |
| `table-layout` | ✅ | `auto`/`fixed`。`auto`ではセル内容の自然幅を測って列幅を決める(ネストしたテーブル・flexの自然幅測定のみ非対応で0扱い) |
| `border-collapse` | ⚠️ | `separate`/`collapse`。`collapse`は見た目の枠線統合のみを行う(CSS2.1 §17.6.2の競合解決を「太い方が勝ち、同幅ならスタイル優先順」で簡略化)。継承プロパティ |
| `border-spacing` | ✅ | `<length>{1,2}`。`border-collapse: collapse`時は0として扱う。継承プロパティ |
| `caption-side` | ⚠️ | `top`/`bottom`。縦書き向けの`left`/`right`は非対応 |
| `empty-cells` | ✅ | `show`/`hide`。`border-collapse: separate`でのみ意味を持つ。継承プロパティ |
| `vertical-align`(セル) | ✅ | 上記[フォント・テキスト](#フォントテキスト)を参照 |

`<colgroup>`/`<col>`の`width`属性・CSS`width`による列幅指定、`rowspan`/`colspan`、`<thead>`のページまたぎ繰り返しに対応。
`rowspan="0"`は1として扱う。

無名テーブルボックスの生成(CSS2.1 §17.2.1 規則2.1・2.2)に対応する。
`display: table`の直下に置かれた`display: table-cell`は無名の行にまとめられ、行やセルにならない子は無名のセルでくるまれる。
行を書かない`display: table` + `display: table-cell`の段組みがそのまま動く。

セルの`min-width`/`max-width`は列幅アルゴリズムに反映される。
`table-layout: auto`では列の自然幅をクランプする形で効くため、表を紙幅に収める比例縮尺の後は`min-width`が保証されない。
`table-layout: fixed`では1行目のセルの指定幅をクランプし、`width: auto`かつ`min-width`のみの指定はその値を列幅として使う。

## リスト

| プロパティ | 対応 | 備考 |
| - | - | - |
| `list-style`(ショートハンド) | ✅ | `type`/`position`/`image`を任意順・任意省略で受け付ける |
| `list-style-type` | ⚠️ | `disc`/`circle`/`square`/`decimal`/`decimal-leading-zero`/`lower-roman`/`upper-roman`/`lower-alpha`(`lower-latin`)/`upper-alpha`(`upper-latin`)/`none`。`cjk-*`/`hiragana`/`katakana`等は非対応。継承プロパティ |
| `list-style-position` | ✅ | `outside`/`inside`。継承プロパティ |
| `list-style-image` | ⚠️ | `none \| url(...)`をパースするが描画には使わない(常に`list-style-type`のテキストマーカーへフォールバック) |

## 生成コンテンツ・カウンタ

| プロパティ | 対応 | 備考 |
| - | - | - |
| `content` | ⚠️ | `::before`/`::after`/`::first-letter`および`@page`のmargin box用。文字列リテラル・`attr()`・`counter()`/`counters()`・`open-quote`/`close-quote`/`no-open-quote`/`no-close-quote`の連結に対応。`url()`による画像挿入は非対応。ブロック子を持つ要素の`::before`/`::after`は生成されない(簡略化) |
| `counter-reset` | ✅ | `none`または`name [<integer>]`の繰り返し |
| `counter-increment` | ✅ | `none`または`name [<integer>]`の繰り返し(値省略時は1) |
| `counter-set` | ❌ | 未実装 |

`counter(page)`/`counter(pages)`によるページ番号は`@page`のmargin box内で使える(`counter(pages)`はストリーミングモードでは総ページ数が確定しないためエラーになる)。

## ページ分割(CSS Fragmentation)

| プロパティ | 対応 | 備考 |
| - | - | - |
| `break-before` / `break-after` | ⚠️ | `auto`/`avoid`(`avoid-page`/`avoid-column`も同義)/`always`(`page`も同義)。`left`/`right`/`recto`/`verso`(見開き制御)、多段組み関連の値は非対応 |
| `break-inside` | ⚠️ | `auto`/`avoid`(`avoid-page`/`avoid-column`も同義) |
| `page-break-before` / `page-break-after` / `page-break-inside` | ✅ | 上記`break-*`のエイリアス(wkhtmltopdf/wicked_pdf資産からの移行用) |
| `orphans` / `widows` | ✅ | 1以上の整数。初期値2 |
| `page`(名前付きページ) | ❌ | `@page intro`のような名前付きページ自体が非対応 |

HTML属性`data-page-break="before|after|avoid"`によるシンタックスシュガーも利用できる。

## Flexbox

`display: flex`のレイアウトは[taffy](https://github.com/DioxusLabs/taffy)へ委譲する。
1ページに収まるflexコンテナはページ分割上アトミック(途中で分割せず、収まらなければ次ページへ送る)。
1ページに収まらない高さのコンテナは、縦に重ならないアイテム群を単位に分割する(詳細は[ページ分割](./pagination.md)を参照)。

| プロパティ | 対応 | 備考 |
| - | - | - |
| `flex-direction` | ✅ | `row`/`row-reverse`/`column`/`column-reverse` |
| `flex-wrap` | ✅ | `nowrap`/`wrap`/`wrap-reverse` |
| `justify-content` | ⚠️ | `normal`(初期値)/`flex-start`(`start`)/`flex-end`(`end`)/`center`/`space-between`/`space-around`/`space-evenly`。`safe`/`unsafe`オーバーフローキーワードは非対応 |
| `align-items` | ✅ | `flex-start`(`start`)/`flex-end`(`end`)/`center`/`baseline`/`stretch` |
| `align-content` | ✅ | `flex-start`(`start`)/`flex-end`(`end`)/`center`/`stretch`/`space-between`/`space-around`/`space-evenly` |
| `align-self` | ✅ | `auto`/`flex-start`(`start`)/`flex-end`(`end`)/`center`/`baseline`/`stretch` |
| `flex-grow` / `flex-shrink` | ✅ | 非負の`<number>`(負値は無効な宣言として無視) |
| `flex-basis` | ⚠️ | `auto`/`content`/`<length-percentage>`。`content`は`auto`と同一視 |
| `flex`(ショートハンド) | ✅ | `none`および`<grow> [<shrink>] [<basis>]`。CSS仕様の既定値規則(`flex: 1`のbasisは`0%`、`flex: <width>`のgrow/shrinkは1)を再現 |
| `gap` / `row-gap` / `column-gap` | ⚠️ | flexコンテナでのみ有効(多段組みの`column-gap`としては機能しない) |
| `order` | ❌ | taffy 0.12系が未対応のため |
| `place-content` / `place-items` / `place-self`(ショートハンド) | ❌ | 未実装。個別のロングハンドを使う |
| `justify-items` / `justify-self` | — | flexアイテムには適用されない(下記) |

### `justify-items`/`justify-self`がflexで効かないのは仕様

CSS Box Alignmentでは`justify-items`/`justify-self`はGrid・ブロックレイアウト用のプロパティで、flexアイテムには適用されない(主軸方向のアイテム個別の配置は`justify-content`と`margin: auto`で表現する、という設計)。
ブラウザも無視する。
レイアウトを委譲しているtaffyでも、これらを参照するのはGridのアルゴリズムだけで、flexboxのアルゴリズムは一切参照しない。

したがってこのエンジンでパースに対応しても見た目は変わらないため、意図的に実装していない。
flexで特定のアイテムだけを寄せたい場合は`margin: auto`を使う(こちらは対応済み):

```css
.item { margin-left: auto; }            /* justify-self: end 相当(主軸の終端へ) */
.item { margin-left: auto; margin-right: auto; }  /* justify-self: center 相当 */
```

## Grid

`display: grid`のレイアウトはFlexboxと同じくtaffyへ委譲する。
1ページに収まらないグリッドは行単位でページ分割される(テーブルと同じ方針。複数行にまたがるアイテムがある境界では分割しない)。

| プロパティ | 対応 | 備考 |
| - | - | - |
| `grid-template-columns` / `grid-template-rows` | ✅ | `none`/`<length>`/`<percentage>`/`fr`/`auto`/`min-content`/`max-content`/`minmax()`/`fit-content()`/`repeat(<整数>\|auto-fill\|auto-fit)`/`[name]`(ライン名)。トラックサイズに`calc()`は非対応 |
| `grid-template-areas` | ✅ | 文字列マトリクス。列数の不一致・非矩形のエリアは不正な値として宣言ごと無視する(仕様通り)。`.`は名前なしセル |
| `grid-auto-columns` / `grid-auto-rows` | ✅ | `<track-size>+` |
| `grid-auto-flow` | ✅ | `row`/`column`/`dense`(併記可) |
| `grid-row-start` / `-end` / `grid-column-start` / `-end` | ✅ | `auto`/`<integer>`/`span <integer>`/`<custom-ident>`/`span <custom-ident>` |
| `grid-row` / `grid-column` / `grid-area` | ✅ | `/`区切りのショートハンド。`grid-area: <name>`で名前付きエリアを指定できる |
| `justify-items` / `justify-self` | ✅ | Gridでのみ意味を持つ(flexアイテムには適用されない、[上記](#justify-itemsjustify-selfがflexで効かないのは仕様)) |
| `align-items` / `align-self` / `justify-content` / `align-content` / `gap` | ✅ | Flexboxと共有(値の範囲は[Flexbox](#flexbox)節を参照) |
| `grid` / `grid-template`(ショートハンド) | ❌ | 個別のロングハンドを使う。トラック定義とエリア定義を1つの構文へ詰め込む複雑な文法のため非対応 |
| `display: inline-grid` | ❌ | `inline-flex`と同じ理由で非対応 |
| subgrid / masonry | ❌ | 未実装 |

`auto`トラックだけで構成したグリッドにコンテナ幅の指定がある場合、余った幅は`auto`トラックへ配分されますが、その配分比率はCSSの規定(`auto`トラックへ均等)と一致しません。
列幅を厳密に決めたい場合は`fr`か長さで指定してください。

## 置換要素(画像)

| プロパティ | 対応 | 備考 |
| - | - | - |
| `object-fit` | ✅ | `fill`/`contain`/`cover`/`none`/`scale-down`。`<img>`にのみ意味を持つ |
| `object-position` | ✅ | `background-position`と同じ値文法。初期値`50% 50%` |

`<img>`はインライン配置・`width`/`height`属性/CSSによるサイズ指定に対応。
対応フォーマットはPNG/JPEG/WebP。
CSSで`width`/`height`の片方だけを指定した場合は内在アスペクト比でもう一方を導出する([`aspect-ratio`](#ボックスモデル)、)。

## 非対応プロパティ一覧

以下は宣言ごと無視される(パースエラー)。
実装が無いだけで、意図的に永久除外と決めたものだけではない。
カテゴリ表の`❌`行も参照。

* ショートハンド: `font`/`place-content`/`place-items`/`place-self`/`text-decoration`の色・線種部分
* 論理プロパティのうち寸法系: `inline-size`/`block-size`/`min-inline-size`/`max-block-size`等(`margin`/`padding`/`inset`/`border`の論理プロパティは対応済み)
* 書字方向: `direction`/`unicode-bidi`/`writing-mode`/`text-orientation`/`text-combine-upright`
* テキスト詳細: `text-decoration-color`/`-style`/`-thickness`/`text-underline-offset`/`text-emphasis-skip`/`tab-size`/`ruby-*`/`text-justify`/`line-break`
* フォント詳細: `font-variant`/`font-stretch`/`font-feature-settings`/`font-variation-settings`/`font-kerning`/`font-display`
* 多段組み: `columns`/`column-count`/`column-width`/`column-rule`/`column-span`/`column-fill`
* 視覚効果: `filter`/`backdrop-filter`/`mix-blend-mode`/`background-blend-mode`/`clip`/`clip-path`/`mask`/`isolation`
* 3D/アニメーション: `perspective`/`transform-style`/`backface-visibility`/`translate`/`rotate`/`scale`(個別プロパティ版)/`transition-*`/`animation-*`/`will-change`
* 枠線・背景の拡張: `border-image-*`/`background-clip`/`background-origin`/`outline-offset`
* オーバーフロー: `overflow-x`/`overflow-y`/`resize`/`scroll-*`/`overscroll-behavior`
* UI/対話: `cursor`/`pointer-events`/`user-select`/`caret-color`/`accent-color`/`appearance`
* その他: `all`/`content-visibility`/`counter-set`/`page`(名前付きページ)/`speak`等の音声メディア系/`zoom`
