# セレクタ・値・at-rule対応表

- [セレクタ](#セレクタ)
- [擬似クラス](#擬似クラス)
- [擬似要素](#擬似要素)
- [値・単位・関数](#値単位関数)
- [色](#色)
- [at-rule](#at-rule)
- [ストリーミングモード固有の制約](#ストリーミングモード固有の制約)

## セレクタ

マッチングはServo由来の[`selectors`](https://crates.io/crates/selectors)クレートに委譲しているため、CSS3セレクタは概ねそのまま使える。

| セレクタ | 対応 | 備考 |
| - | - | - |
| タイプ(`p`)・ユニバーサル(`*`) | ✅ | |
| クラス(`.foo`)・ID(`#foo`) | ✅ | |
| 属性(`[a]`/`[a=v]`/`[a^=v]`/`[a$=v]`/`[a*=v]`/`[a~=v]`/`[a\|=v]`) | ✅ | 大文字小文字を無視する`i`フラグも使える |
| 子孫(空白)・子(`>`) | ✅ | |
| 隣接兄弟(`+`)・一般兄弟(`~`) | ✅ | |
| セレクタリスト(`,`) | ✅ | |
| ネスト(`.a { .b { } }` / `& .b` / `&.b` / `> .b`) | ✅ | CSS Nesting。`&`を省いたセレクタは`& .b`、先頭がコンビネータのものは`& > .b`として扱う。`&`は`:is(親)`に置き換わるので詳細度は親のうち最も高いものになる。ネストしたルールの後ろに書いた宣言は、そのルールより後ろの順序でカスケードに参加する。ネストした`@media`等のat-ruleは非対応(ブロックごと無視) |
| 名前空間(`ns\|E`) | ❌ | `@namespace`自体が非対応 |

## 擬似クラス

| 擬似クラス | 対応 | 備考 |
| - | - | - |
| `:root` | ✅ | |
| `:first-child` / `:last-child` / `:only-child` | ✅ | |
| `:nth-child()` / `:nth-last-child()` | ✅ | ストリーミングモードでは`:nth-last-child()`の結果が変わる(下記参照) |
| `:first-of-type` / `:last-of-type` / `:only-of-type` / `:nth-of-type()` / `:nth-last-of-type()` | ✅ | ストリーミングモードでは`:first-of-type`/`:nth-of-type()`以外の結果が変わる(下記参照) |
| `:empty` | ✅ | |
| `:not()` | ✅ | |
| `:is()` / `:where()` | ✅ | 引数リストは寛容(未対応のセレクタが混ざっていてもその項だけを捨てる)。詳細度は`:is()`が引数のうち最も高いもの、`:where()`は常に0 |
| `:has()` | ✅ | 子孫・`>`・`+`・`~`のいずれも書ける。詳細度は`:is()`と同じ規則。入れ子(`:has()`の中の`:has()`)と擬似要素は仕様どおり書けない。ストリーミングモードでは`:has(~ ...)`が非マッチ(下記参照) |
| `:hover` / `:active` / `:focus` / `:focus-within` / `:focus-visible` / `:target` / `:enabled` / `:disabled` / `:checked` / `:visited` | ⚠️ | パースは通るが常に非マッチ。対話状態を持たない静的なPDF出力では意味を持たないため |
| `:link` / `:any-link` | ✅ | `href`を持つ`<a>`にマッチする |
| `:host` / `:host()` | ⚠️ | パースは通るが常に非マッチ。シャドウツリーを持たないため。Tailwind v4がテーマ変数を`:root, :host`に、preflightを`html, :host`に書くので、セレクタリストの他の項が捨てられないようにするためのもの。`:host-context()`はパースエラー |

非対応の擬似要素(`::first-line`等)がセレクタに含まれるとルール全体が捨てられる。
一方`:hover`や`::placeholder`のようにパースが通るものは、ルールとしては生き残った上でマッチしない。
`:is()` / `:where()`の引数リストだけは例外で、未対応の項があってもその項が捨てられるだけでルールは生きる。

## 擬似要素

| 擬似要素 | 対応 | 備考 |
| - | - | - |
| `::before` / `::after` | ⚠️ | `content`による生成テキストのみ。ホスト要素の計算スタイルをそのまま流用して描画し、擬似要素専用のボックススタイル(margin/padding/display等)は持たない。ブロック子を持つ要素では生成されない |
| `::first-letter` | ⚠️ | `font-family`/`font-size`/`font-weight`/`font-style`/`color`/`text-decoration-line`/`text-transform`のみ上書きできる(`float`・box model系は非対応) |
| `::first-line` | ❌ | パースエラー |
| `::backdrop` / `::file-selector-button` / `::placeholder` / `::selection` / `::marker` | ⚠️ | パースは通るが常に非マッチ。静的なPDFには最上位レイヤー・ファイル入力・空のフォーム欄・選択範囲がなく、`::marker`も専用のスタイルを持たない(リストマーカーは`list-style-*`で決まる)ため。Tailwind v4のpreflightにある`*, ::after, ::before, ::backdrop, ::file-selector-button { ... }`のようなセレクタリストの他の項が捨てられないようにするためのもの |
| `::-webkit-input-placeholder`等のベンダー接頭辞付き | ❌ | パースエラー |

## 値・単位・関数

### 長さ

| 単位 | 対応 |
| - | - |
| `px` / `em` / `rem` | ✅ |
| `mm` / `cm` / `in` / `pt` / `pc` / `Q` | ✅ |
| `%`(パーセンテージを取るプロパティで) | ✅ |
| 単位なしの`0` | ✅ |
| `ex` / `ch` / `vw` / `vh` / `vmin` / `vmax` / `lh` | ❌ |

絶対単位は1インチ = 96pxとして解釈する(`10mm`は37.795px)。
印刷向けに寸法を実寸で書けるので、`@page { size: 210mm 297mm; margin: 15mm; }`のような指定がそのまま通る。

`@page`の`size`だけは例外的にページサイズのキーワード(`a4`/`letter`等)と`landscape`/`portrait`を受け付ける。

### 角度(`transform`専用)

`deg` / `rad` / `grad` / `turn`、および単位なしの`0`に対応 ✅。

### CSS全体キーワード

| キーワード | 対応 | 備考 |
| - | - | - |
| `inherit` / `initial` / `unset` | ✅ | ショートハンドを含むすべてのプロパティで使える。`inherit`は親の計算値をそのまま引き継ぐ(`em`は親の側で解決済みの値になる)。`var()`のフォールバック(`var(--x, inherit)`)でも使える。`::before`/`::after`の`content`はどのキーワードでも`normal`(生成しない)になり、`counter-reset`/`counter-increment`はそれまでの宣言を打ち消すだけになる。`@page`の中と`::first-letter`では無視される |
| `revert` / `revert-layer` | ❌ | 宣言ごと無視される |

### 関数

| 関数 | 対応 | 備考 |
| - | - | - |
| `calc()` | ⚠️ | `+`/`-`/`*`/`/`と括弧・`calc()`のネスト。項に使えるのは長さ(絶対単位・`em`/`rem`)・`%`・数値のみ。長さ・パーセンテージを取るプロパティ全般で使える |
| `min()` / `max()` / `clamp()` | ❌ | |
| `var()` | ✅ | カスタムプロパティ(`--foo`)は他のプロパティと同じく要素ごとにカスケード(詳細度・記述順・`!important`・`style`属性)し、継承する。`var()`は計算値の時点でその要素のカスタムプロパティで置換してからパースする。フォールバック(`var(--x, 10px)`、入れ子可)・カスタムプロパティ同士の参照(循環は無効)・`initial`/`inherit`に対応。解決できない`var()`を含む宣言は計算値の時点で無効になり、そのプロパティは継承値(継承されるプロパティ)か初期値になる。`@page`(`size`・余白・マージンボックス)には要素がないため`:root`(`<html>`)の値で解決し、`@page`内に書いたカスタムプロパティは参照できない。`::before`/`::after`の`content`は要素の値を使う。`@property`と`!important`付きの通常プロパティ(`color: var(--c) !important`)は未対応 |
| `url()` | ✅ | `background-image`/`list-style-image`/`@font-face`の`src`/`@import`。相対URLは`<base href>`または入力元を基準に解決する |
| `attr()` | ⚠️ | `content`の中でのみ使える |
| `counter()` / `counters()` | ✅ | `content`の中。第2引数のスタイルは`list-style-type`の値集合 |
| `linear-gradient()`等のグラデーション | ❌ | |
| `env()` / `image-set()` / `element()` | ❌ | |

### 色

| 記法 | 対応 |
| - | - |
| 名前付き色(`red`等のCSS色キーワード) | ✅ |
| `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa` | ✅ |
| `rgb()` / `rgba()`(カンマ区切り・スペース区切りとも) | ✅ |
| `hsl()` / `hsla()` / `hwb()` | ✅ |
| `lab()` / `lch()` / `oklab()` / `oklch()` | ✅ (sRGBへ変換して描画) |
| `currentcolor` / `transparent` | ✅ |
| `color()`(`color(display-p3 ...)`等) | ❌ |
| `color-mix()` | ⚠️ (下記) |
| 相対色構文(`rgb(from ...)`) | ❌ |

アルファ付きの色は塗り・背景ともPDFのExtGStateで透過描画する。

#### `color-mix()`

`color-mix(in <色空間> [<色相の回し方> hue]?, <色> <割合>?, <色> <割合>?)`に対応する。
割合の正規化(合計が100%を超えるときは比率だけが効き、満たないときは足りないぶんだけ結果が透明になる)と、アルファの事前乗算も仕様どおり行う。

* 色空間: `srgb` / `srgb-linear` / `lab` / `oklab` / `xyz`(`xyz-d65`) / `hsl` / `hwb` / `lch` / `oklch`
* 色相の回し方: `shorter`(既定) / `longer` / `increasing` / `decreasing`
* 入れ子にできる(深さ16まで)

以下は非対応で、書くとその宣言だけが無視される。

* `display-p3` / `a98-rgb` / `prophoto-rgb` / `rec2020`。出力先がPDFのDeviceRGBなので、受け付けてもsRGBへ丸められるだけで指定した意味にならない
* オペランドの`currentcolor`。`currentcolor`はカスケードの後(その要素の`color`が決まってから)解決するのに対し、混色はパース時に済ませるため、この時点では値が分からない
* `xyz-d50`(白色点の変換が要るため)

## at-rule

| at-rule | 対応 | 備考 |
| - | - | - |
| `@media` | ⚠️ | メディアタイプのみを評価する。`screen`(および`not screen`以外の否定)はブロックごと無視し、`print`/`all`/タイプ省略は適用する。`(min-width: ...)`のような特性クエリは評価せず読み飛ばす(=タイプさえ合致すれば中身が適用される) |
| `@page` | ⚠️ | `size`(キーワード/`<length>{1,2}`/`landscape`/`portrait`)と`margin`系に対応。`:first`/`:left`/`:right`擬似クラス単体に対応し、名前付きページ(`@page intro`)・`:blank`・複合擬似クラス(`:first:left`)は非対応 |
| `@page`内のmargin box | ⚠️ | `@top-left-corner`/`@top-left`/`@top-center`/`@top-right`…の16種すべて。`content`のテキスト描画のみで、背景色・枠線等の装飾は非対応。`counter(page)`/`counter(pages)`でページ番号を出力できる(`counter(pages)`はストリーミングモードでは非対応) |
| `@font-face` | ⚠️ | `font-family`/`src`/`unicode-range`/`font-weight`/`font-style`ディスクリプタに対応(`src`の`local()`・`format()`/`tech()`付き`url()`も受理)。`font-display`等その他のディスクリプタは無視。フォントファイルはTTF/OTFのみで、WOFF/WOFF2は非対応 |
| `@import` | ✅ | ネスト(深さ上限16、超過分はその1件だけ無視)・循環参照の検出に対応。`@import url(...) screen;`のようなメディアクエリ条件は評価せず常に取り込む |
| `@charset` | ❌ | 入力はUTF-8前提 |
| `@layer` | ⚠️ | ブロックの中のルールを書かれた順にトップレベルへ展開する(`@layer a { .x {} }`は`.x {}`と同じ)。レイヤーの優先順位は実装せず、通常のカスケード(詳細度・後勝ち)で決まる。`@layer a, b;`の順序宣言は無視する。Tailwind v4のように出力全体を`@layer`で包むCSSをそのまま渡せる |
| `@supports` / `@keyframes` / `@namespace` / `@counter-style` / `@container` / `@property` | ❌ | ブロックごと無視される |

## ストリーミングモード固有の制約

`Mode::Batch`(一括処理)ではDOM全体が揃っているため下記の制約は無い。
`Mode::Streaming`でのみ以下が適用される。

`<body>`直下のトップレベル要素は、次の兄弟が現れた時点で確定として処理します。
そのため「この先に同じ型の要素が続くか」が要るセレクタだけ、`<body>`直下の要素に限って結果が変わります(その要素の内側は部分木が揃っているので変わりません)。

* `:last-of-type`/`:only-of-type`/`:nth-last-child()`/`:nth-last-of-type()`/`:has(~ ...)`は、余分にマッチしたり取りこぼしたりします。
  これらを使うと警告が出ます
* `:last-child`・`:empty`・`:has()`の子孫/直後の兄弟は、確定の条件と一致するのでバッチと同じ結果になります
* `+`/`~`・`:first-child`/`:nth-child()`/`:first-of-type`/`:nth-of-type()`/`:only-child`もバッチと同じ結果になります。
  これらを使っている文書では、処理済みのトップレベル要素を子孫だけ解放し、要素そのものを兄弟として残すためです
  (残るのはトップレベル要素1個につき1ノードなので、解放できる量はほぼ変わりません)
* `<body>`開始後の`<style>`タグはエラー: `EngineError::UnsupportedInStreamingMode`を返す。
  黙って見た目が崩れるのを避けるため、`<style>`は`<head>`に集約する
* `position: absolute`/`fixed`は無視される
* `counter(pages)`(総ページ数)は使えない
