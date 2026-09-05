# 画像

`<img>`とCSSの`background-image`で画像を埋め込めます。

| 対応フォーマット | PNG / JPEG / WebP / SVG |
|---|---|
| `src`に書けるもの | ローカルの相対パス・絶対パス、`http(s)`のURL、`data:` URI |

GIFは非対応です。

## SVG

SVG(`.svg`と、gzip圧縮された`.svgz`)はラスタライズせず、ベクタのままPDFへ
埋め込みます。拡大しても解像度に依存せず、PDFのサイズもピクセル数ではなく
図形の数で決まります。パース・正規化は[usvg]、PDFの描画命令への変換は
[svg2pdf](どちらも[typst]由来)が行います。

**参照して使う形だけに対応します。** `<img src>`と`background-image: url()`の
どちらでも使えますが、HTMLに直接書いたインラインの`<svg>`要素は描画しません
(後述)。

```html
<img src="logo.svg" width="120">
<div style="background-image: url(pattern.svg)"></div>
```

`data:` URIも使えます。SVGでは`;base64`を付けない書き方(パーセント
エンコード)が一般的なので、どちらの形も受け付けます。

```html
<img src="data:image/svg+xml,%3Csvg%20xmlns%3D...%3E">
<img src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0...">
```

```css
/* CSSの url() でも同じ */
background-image: url("data:image/svg+xml,%3Csvg%20xmlns%3D...%3E");
```

フォーマットの判定は**中身のバイト列**で行います。拡張子や`data:`が名乗る
mime typeは見ないので、`.txt`に入ったSVGも描けますし、`image/png`と名乗った
SVGも描けます(逆に、`.svg`という名前のPNGはPNGとして扱います)。

寸法の決め方・キャッシュ・エラー時の扱いはラスタ画像と同じです。SVGの
`width`/`height`(無ければ`viewBox`)が内在サイズになります。ラスタ画像と
違って内在サイズは小数になりうるので、そのまま(丸めずに)扱います。
`object-fit: contain`のようにアスペクト比で決まる指定が、`width="40.6"`の
ようなSVGでもずれません。

### object-fit / object-position

ラスタ画像と同じように効きます。SVGはPDFへ単位正方形に正規化された形で
入るため、`fill`/`contain`/`cover`/`none`/`scale-down`と`object-position`の
計算はラスタ画像と完全に共通の実装が使われます。

```css
img.logo {
  width: 200px; height: 80px;
  object-fit: contain;      /* 比を保って収める(余白ができる) */
  object-position: 0% 50%;  /* 左寄せ */
}
```

`cover`のようにはみ出す指定でも、描画はcontent boxでクリップされます。

### SVG内のテキストとフォント

**既定では、SVG内の`<text>`は描画されません。** パスにもならず、何も出ません
(そのSVGの他の図形は描かれます)。`<text>`を含むSVGを見つけたら警告します。

`svg-text` featureを有効にすると、グリフのまま(選択・検索できるテキストと
して)埋め込みます。使えるフォントは**文書が使うフォントそのもの**で、
解決の仕方はHTML側と揃えてあります。

| SVGの`font-family` | 使われるフォント |
|---|---|
| フォント内部のfamily名(`DejaVu Sans`等) | そのフォント |
| `@font-face`で宣言した名前 | そのフォント |
| `serif` / `sans-serif` / `monospace` | `--serif-font` / `--gothic-font` / `--mono-font`(未指定なら既定フォント) |
| 指定なし | 文書の既定フォント(`--font`で最初に渡したもの) |
| 文書が持っていない名前 | 文書の既定フォント |

```css
@font-face { font-family: BrandFace; src: url(brand.ttf); }
```

```xml
<!-- `@font-face`で宣言した名前でも、フォント内部のfamily名でも引ける -->
<text font-family="BrandFace">売上</text>
```

* **SVGのためにシステムフォントを探し直すことはしません。** 文書が持って
  いないフォントがSVGにだけ現れることはなく、代わりに文書の既定フォントへ
  落ちます
* 文書側とSVG側では、同じフォントファイルでもサブセットは別になります
  (必要なグリフが違うため)。埋め込みはフォント1つにつき1回です
* `svg-text`が既定オフなのは、有効にすると[rustybuzz]・resvg等25クレートが
  依存に加わるためです(svg2pdfの`text` featureがそれらを要求します)。
  SVGにテキストが無いなら足す必要はありません

[rustybuzz]: https://github.com/harfbuzz/rustybuzz

### インラインSVGは描画しません

HTMLに直接書いた`<svg>`要素は、サブツリーごと描画対象から外します
(UAスタイルシートの`svg { display: none }`)。中のテキストが本文へ
流れ込むこともありません。文書内に1つでもあれば警告します。

```html
<!-- 描画されない -->
<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
  <rect width="40" height="20" fill="red"/>
</svg>

<!-- こう書けば描画される -->
<img src="logo.svg" width="40" height="20">
<img src="data:image/svg+xml,%3Csvg%20...%3E" width="40" height="20">
```

対応させるにはHTMLのDOMからSVGのXMLを組み直してusvgへ渡す必要があり、
属性名の大小(`viewBox`等)・CSSの継承・`currentColor`をどう扱うかが
外部ファイルの参照とは別の問題になります。今のところ「参照して使う」形に
絞っています。

### その他の制限

* SVGフィルタ(`<filter>`)は非対応です。ラスタライズを避けるため、
  フィルタを解決するための機能を入れていません
* SVG内に埋め込まれたラスタ画像(`<image>`)は描画されません
* `--grayscale`はSVGには効きません(色が個々の描画命令の中にあるため)。
  指定すると警告し、SVGだけ色のまま残ります
* SVGの中の`<image href="...">`のような外部参照は解決しません。参照ごとに
  警告を出して無視します。SVGの中からのファイル読み出しは`<img>`側の
  封じ込め(基準ディレクトリ・`--allow-path`・`--disable-local-file-access`)を
  迂回してしまうため、経路自体を塞いでいます。`data:` URIは対象外
  (SVG自身の中で完結しているため許可されます)

`svg` featureを切る(`--no-default-features`)とusvgを引き込まなくなり、
SVGの参照はデコード失敗として扱われます。

[usvg]: https://github.com/linebender/resvg
[svg2pdf]: https://github.com/typst/svg2pdf
[typst]: https://typst.app/

## `<img>`

```html
<img src="logo.png" width="120">
<img src="https://example.com/chart.png" alt="売上推移">
<img src="data:image/png;base64,iVBORw0…">
```

* `<img>`はインラインの置換要素として行に載ります。独立した行にしたい場合は`display: block`を指定してください
* `width`/`height`属性とCSSの`width`/`height`に対応します。どちらも無指定なら画像の内在サイズを使い、片方だけ指定すればアスペクト比を保って他方を導出します
* 取得やデコードに失敗した画像は、その要素だけ空として扱い、文書全体の生成は止めません(`--load-media-error-handling abort`で中断させることもできます)
* 同じ画像を何度使っても、取得・デコード・PDFへの埋め込みは初回の1回だけです

## `object-fit` / `object-position`

指定した枠に対して画像をどう収めるかを制御します。

```css
img.thumb {
  width: 120px;
  height: 80px;
  object-fit: cover;          /* fill | contain | cover | none | scale-down */
  object-position: 50% 50%;
}
```

## 背景画像

```css
.watermark {
  background-image: url("stamp.png");
  background-position: center;
  background-size: contain;
  background-repeat: no-repeat;
}
```

`background-image`に書けるのは`url()`だけです。
`linear-gradient()`などのグラデーション関数と、カンマ区切りの複数背景は非対応です。
既定では画像の内在サイズでタイル配置されます。

`border-radius`と背景画像を併用した場合、角丸によるクリップは行われません(角丸は背景色の塗りにのみ効きます)。

## リモート画像の取得

既定では無効です。
`--allow-remote-assets`で明示的に有効化します。

```sh
sghtmltopdf report.html --allow-remote-assets
```

有効にした場合も、グローバルに到達可能でない宛先へのリクエストは常にブロックされます。
判定は「グローバルなユニキャストだけを通す」方針で、次を拒否します。

| 種別 | 範囲 |
|---|---|
| ループバック | `127.0.0.0/8`、`::1` |
| プライベート | `10/8`、`172.16/12`、`192.168/16`、`fc00::/7` |
| リンクローカル | `169.254/16`(クラウドのメタデータ`169.254.169.254`を含む)、`fe80::/10` |
| CGNAT | `100.64.0.0/10`(クラウドの内部ロードバランサ等) |
| その他の非グローバル | `0.0.0.0/8`、`192.0.0.0/24`、`198.18.0.0/15`、`240.0.0.0/4`、マルチキャスト、ドキュメント用 |
| IPv6の特殊用途 | Teredo `2001::/32`、`2001:db8::/32`、ORCHIDv2 `2001:20::/28`、`100::/64` |

IPv4を埋め込むIPv6表記(IPv4-mapped `::ffff:a.b.c.d`、IPv4-compatible `::a.b.c.d`、NAT64 `64:ff9b::/96`、6to4 `2002::/16`)は、埋め込まれたIPv4側で判定します。
これらを素通しするとIPv4側のフィルタを迂回できてしまうためです。

判定は名前解決の結果に対して行うため、DNSリバインディングやリダイレクト経由の迂回も同じ仕組みで防いでいます。

ポート番号は制限しません。
内部サービスはプライベートIP上にあり、そこは上の判定で塞がっています。
公開IPに対する非標準ポート(CDNやAPIの`8080`など)は正当な用途があるため、塞ぐと実用を損なうわりに得るものがありません。

信頼できないHTMLを変換する場合は、`--allow-path`でローカル参照の範囲も併せて絞ってください。

```sh
sghtmltopdf untrusted.html --allow-path /var/app/assets
```

## JPEGはそのまま埋め込まれる

JPEGはデコードせず、サイズ情報だけを読んでPDFへそのまま(DCTDecodeとして)埋め込みます。
再エンコードしないので画質は落ちず、変換も速くなります。

その代わり、`--grayscale`を指定してもJPEGとCMYK画像はカラーのまま残ります(デコーダを持たないため)。
グレースケール化が必要な場合は、変換前の画像をグレースケールにしておいてください。

PNGとWebPはフルデコードし、アルファチャンネルがあれば透過画像として埋め込みます。

## 画像を一切読み込まない

```sh
sghtmltopdf invoice.html --no-images
```

`<img>`とCSSの`background-image`の両方を読み込まなくなります。
