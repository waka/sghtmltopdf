# 対応していないこと

個別のCSSプロパティの可否は[プロパティ対応表](../supports/properties.md)を、wkhtmltopdfのオプション単位の可否は[オプション対応表](../migration/wkhtmltopdf-options.md)を参照してください。

## JavaScriptを実行しない

`<script>`は読み飛ばされます。

そのため次のような使い方はできません。

* JSでDOMを組み立ててからPDF化する(SPAのページをそのまま出す等)
* Chart.jsなどでクライアント描画したグラフを含める
* JSでページ番号やヘッダーを差し込む(→ [プレースホルダ](../usage/cli/reference.md#ヘッダーフッター)を使ってください)

グラフを載せたい場合は、サーバ側で画像(PNG)を生成して`<img>`で埋め込むか、CSSで描ける範囲の表現に置き換えてください。

## 入力は1つのHTML

複数のHTMLファイルを並べて1つのPDFへ結合することはできません(wkhtmltopdfの位置引数に相当する機能がありません)。
表紙は`--cover`、目次は`--toc`で個別に指定します。

既存のPDF同士の結合・分割・ページ抽出も対象外です。

## PDFの機能

| 機能 | 状況 |
|---|---|
| アウトライン(しおり・ブックマーク) | 非対応。文書内の目次は`--toc`で作れます |
| 入力可能なフォーム(AcroForm) | 非対応。`<input>`等は見た目だけ描画します |
| 暗号化・パスワード・電子署名 | 非対応 |
| PDF/A・PDF/X などの規格準拠 | 非対応 |
| タグ付きPDF(アクセシビリティ) | 非対応 |
| 添付ファイル・注釈(リンク以外) | 非対応。リンク注釈のみ対応 |

リンク(`<a href>`)は、外部URL・文書内の`#id`ともPDFの注釈になります。

## 画像・フォントの形式

* 画像はPNG / JPEG / WebP / SVGのみ。GIFは非対応です
* SVGは`<img>`と`background-image`からの参照のみ(ファイル・`data:` URIどちらも可)。HTMLに直接書いたインラインの`<svg>`要素は描画せず、見つけたら警告します
* SVG内の`<text>`は`svg-text` featureを有効にした場合だけ描画します(使えるフォントは文書と同じもので、SVGのためにシステムフォントを探し直すことはしません)。`<filter>`と`<image>`は非対応です([画像](../supports/images.md#svg)を参照)
* フォントはTTF / OTFのみ。WOFF / WOFF2は非対応です
* カラーフォントは埋め込みビットマップ(`CBDT`/`CBLC`・`sbix`)と`COLR`/`CPAL` v0のみ。COLRv1(グラデーション)とOpenType SVGは非対応です。絵文字は[フォント](../supports/fonts.md#絵文字)を参照してください
* `--grayscale`を指定しても、JPEGとCMYK画像・SVGはカラーのまま残ります

## CSSの主な制限

機能単位では以下が非対応です。

* 縦書き(`writing-mode`/`text-orientation`)と`direction`による右横書き(`margin-inline`等の論理プロパティは`horizontal-tb`+LTRの固定写像としてのみ対応)
* 多段組み(`columns`/`column-count`)
* グラデーション(`linear-gradient()`等)と複数背景
* 相対色構文(`rgb(from ...)`)と`color()`(`color-mix()`は対応済み)
* アニメーション・トランジション・`filter`(静的な出力のため)
* `position: sticky`、`display: inline-flex`/`inline-grid`、subgrid
* `::first-line`、`::marker`(`:is()`/`:where()`/`:has()`は対応済み)

値の文法では、`calc()`と括弧のネストが32段までです。
それより深い値は無効として宣言ごと無視します。
再帰的にパースするため、深さの上限を設けないと信頼できないCSSでスタックを使い切らせることができてしまうためです。

## 空白文字の扱い

畳み込みの対象はCSS Text 3が定める空白(space・tab・改行)だけです。
`&nbsp;`(U+00A0)やthin space(U+2009)などは畳み込まれない普通の文字として、
フォント本来の字幅で描かれます。改行してよい位置はUAX #14の行分割クラスに従い、
`&nbsp;`・narrow no-break space・figure space・word joinerの前後では改行しません
(`word-break: break-all`を指定した場合も同様です)。

`<wbr>`(と、それと同じ意味を持つU+200B ZERO WIDTH SPACE)は「ここで改行してよい」という
指定として扱います。幅は増えず、PDFのテキスト層にも文字は残りません
(`<wbr>`を挟んでも抽出結果は繋がったままです)。

この範囲での既知の制限は以下です。

* `text-align: justify`が行を伸ばす際、`&nbsp;`は伸縮の対象になりません(通常の空白のみ)
* U+2028/U+2029は本来の強制改行ではなく、通常の空白と同じ扱いになります
* 行末に来た空白の「ぶら下がり」(hanging)は行いません

## ストリーミングモード固有の制限

`--streaming`を使う場合は、総ページ数(`counter(pages)`・`[topage]`)や目次(`--toc`)などが使えなくなります。
詳細は[ストリーミングモード](../usage/cli/streaming.md)を参照してください。

## 今後について

JavaScriptエンジンの組み込みは、必要性が出てきた段階での検討事項として残してあります。
上記のうちPDFのアウトラインなど、設計上の非目標ではないものは将来対応する可能性があります。
