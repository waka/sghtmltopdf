# wkhtmltopdfオプション対応表

wkhtmltopdf 0.12.6の`--extended-help`(公式マニュアル<https://wkhtmltopdf.org/usage/wkhtmltopdf.txt>)に載る全オプションの対応状況です。
セクション区切りと並びは公式マニュアルに合わせてあります。

| 記号 | 意味 |
|---|---|
| ✅ 対応 | wkhtmltopdfと同じ名前・同じ意味で使える |
| ❌ 非対応 | 実装しない。指定するとと代替メッセージを出力してexit 1で終了する(黙って無視しない) |

移行時に引っかかりやすい挙動の違いは[wkhtmltopdfからの移行](wkhtmltopdf.md)にまとめてあります。

## コマンドライン形式の違い

wkhtmltopdfは表紙と目次を位置引数で指定します。

```
wkhtmltopdf cover cover.html toc page.html out.pdf          # wkhtmltopdf
sghtmltopdf --cover cover.html --toc page.html -o out.pdf   # sghtmltopdf
```

sghtmltopdfは入力を1つのHTMLに限定し、表紙・目次は`--cover <path>`・`--toc`オプションで指定します。
複数HTMLの結合(wkhtmltopdfが位置引数を並べてできること)には対応していません。

そのため、位置引数由来の`--exclude-from-outline`/`--include-in-outline`(入力ページ単位で目次から除外する)も対象外です。

## Global Options

| オプション | 方針 | 備考 |
|---|---|---|
| `--collate` / `--no-collate` | ❌ 非対応 | 印刷時の丁合。PDF生成に意味を持たない |
| `--cookie-jar <path>` | ❌ 非対応 | 認証付きフェッチはスコープ外 |
| `--copies <number>` | ❌ 非対応 | 同上（印刷用） |
| `-d, --dpi <dpi>` | ✅ 対応 | 既定96 |
| `-H, --extended-help` | ❌ 非対応 | `--help`に一本化する |
| `-g, --grayscale` | ✅ 対応 | |
| `-h, --help` | ✅ 対応 | clapが生成 |
| `--htmldoc` / `--manpage` / `--readme` / `--license` | ❌ 非対応 | ドキュメントは`docs/`とREADMEで提供する |
| `--image-dpi <integer>` | ❌ 非対応 | 画像のリサンプリングを行わないため |
| `--image-quality <integer>` | ❌ 非対応 | JPEGデコーダ/エンコーダを持たず、そのまま埋め込むため |
| `--log-level <level>` | ✅ 対応 | `none`/`error`/`warn`/`info` |
| `-l, --lowquality` | ❌ 非対応 | WebKitのラスタライズ品質設定に相当するものが無い |
| `-B, --margin-bottom <unitreal>` | ✅ 対応 | |
| `-L, --margin-left <unitreal>` | ✅ 対応 | 既定値が違う(下記) |
| `-R, --margin-right <unitreal>` | ✅ 対応 | 既定値が違う(下記) |
| `-T, --margin-top <unitreal>` | ✅ 対応 | |
| `-O, --orientation <orientation>` | ✅ 対応 | `Portrait`/`Landscape` |
| `--page-height <unitreal>` | ✅ 対応 | |
| `-s, --page-size <Size>` | ✅ 対応 | A4/A3/A5/Letter/Legal |
| `--page-width <unitreal>` | ✅ 対応 | |
| `--no-pdf-compression` | ✅ 対応 | 現状は常時Flate圧縮 |
| `-q, --quiet` | ✅ 対応 | `--log-level none`と同義 |
| `--read-args-from-stdin` | ❌ 非対応 | stdinはHTML入力に使うため衝突する |
| `--title <text>` | ✅ 対応 | PDF Info辞書。未指定時は`<title>`を採用 |
| `--use-xserver` | ❌ 非対応 | Xサーバに依存しない |
| `-V, --version` | ✅ 対応 | |

マージンの既定値: wkhtmltopdfは四辺10mm(`--extended-help`には左右の既定値しか載っていないが、実際は上下も10mm)だが、sghtmltopdfの現在の既定は四辺96px(＝1in＝25.4mm)。
既存の出力を変えないため既定値は変更しない。
移行時は`--margin-*`を明示すること。

## Outline Options

PDFのアウトライン(ブックマーク)自体が非対応のため、このセクションはすべて非対応です。
文書内の目次は`--toc`で作れます。

| オプション | 方針 | 備考 |
|---|---|---|
| `--outline` / `--no-outline` | ❌ 非対応 | PDFブックマーク未実装 |
| `--outline-depth <level>` | ❌ 非対応 | 同上 |
| `--dump-outline <file>` | ❌ 非対応 | 同上 |
| `--dump-default-toc-xsl` | ❌ 非対応 | XSLTを使わない |

## Page Options

| オプション | 方針 | 備考 |
|---|---|---|
| `--allow <path>` | ✅ 対応 | ローカル読み込みを許可するディレクトリ。sghtmltopdfでの綴りは`--allow-path`で、`--allow`は別名として受ける。サーバモードでは特に重要 |
| `--background` / `--no-background` | ✅ 対応 | |
| `--bypass-proxy-for <value>` | ❌ 非対応 | プロキシ非対応 |
| `--cache-dir <path>` | ❌ 非対応 | フェッチキャッシュは持たない(必要になれば別途) |
| `--checkbox-checked-svg` / `--checkbox-svg` / `--radiobutton-checked-svg` / `--radiobutton-svg` | ❌ 非対応 | SVG自体は描画できるが、フォーム要素の見た目の差し替えには対応しない(内蔵の描画で再現する) |
| `--cookie <name> <value>` | ❌ 非対応 | 認証付きフェッチはスコープ外 |
| `--custom-header <name> <value>` / `--custom-header-propagation` | ❌ 非対応 | 同上 |
| `--debug-javascript` / `--no-debug-javascript` | ❌ 非対応 | JS非対応 |
| `--default-header` | ✅ 対応 | ページ名+番号の既定ヘッダ。簡易オプションのショートカット |
| `--encoding <encoding>` | ✅ 対応 | 判定順は BOM > `--encoding` > `<meta charset>` > UTF-8 |
| `--disable-external-links` / `--enable-external-links` | ✅ 対応 | リンク注釈 |
| `--disable-forms` / `--enable-forms` | ❌ 非対応 | 入力可能なPDFフォーム(AcroForm)は作らない |
| `--images` / `--no-images` | ✅ 対応 | |
| `--disable-internal-links` / `--enable-internal-links` | ✅ 対応 | |
| `-n, --disable-javascript` / `--enable-javascript` | ❌ 非対応 | JS実行は設計上の非目標 |
| `--javascript-delay <msec>` | ❌ 非対応 | 同上 |
| `--keep-relative-links` / `--resolve-relative-links` | ✅ 対応 | リンク注釈のURL解決。`<base href>`と併せて実装 |
| `--load-error-handling <handler>` | ✅ 対応 | `abort`/`ignore`。`skip`は入力が1つなので無し |
| `--load-media-error-handling <handler>` | ✅ 対応 | 画像・フォント・CSSの取得失敗 |
| `--disable-local-file-access` / `--enable-local-file-access` | ✅ 対応 | 既存`--allow-remote-assets`と整理 |
| `--minimum-font-size <int>` | ✅ 対応 | |
| `--exclude-from-outline` / `--include-in-outline` | ❌ 非対応 | 入力が1つなので意味を持たない |
| `--page-offset <offset>` | ✅ 対応 | ページ番号の起点 |
| `--password` / `--username` | ❌ 非対応 | HTTP認証はスコープ外 |
| `--disable-plugins` / `--enable-plugins` | ❌ 非対応 | プラグイン機構が無い |
| `--post <name> <value>` / `--post-file <name> <path>` | ❌ 非対応 | URL入力時のPOSTはスコープ外 |
| `--print-media-type` / `--no-print-media-type` | ❌ 非対応 | 常に印刷メディア扱い |
| `-p, --proxy <proxy>` / `--proxy-hostname-lookup` | ❌ 非対応 | プロキシ非対応 |
| `--run-script <js>` | ❌ 非対応 | JS非対応 |
| `--disable-smart-shrinking` / `--enable-smart-shrinking` | ❌ 非対応 | WebKit固有の縮小戦略 |
| `--ssl-crt-path` / `--ssl-key-password` / `--ssl-key-path` | ❌ 非対応 | クライアント証明書はスコープ外 |
| `--stop-slow-scripts` / `--no-stop-slow-scripts` | ❌ 非対応 | JS非対応 |
| `--disable-toc-back-links` / `--enable-toc-back-links` | ✅ 対応 | 見出し→目次への逆リンク |
| `--user-style-sheet <path>` | ✅ 対応 | ユーザーオリジンのCSS |
| `--viewport-size <size>` | ❌ 非対応 | ビューポート概念が無い |
| `--window-status <status>` | ❌ 非対応 | JS非対応 |
| `--zoom <float>` | ✅ 対応 | |

## Headers And Footer Options

このセクションはすべて対応しています。
JSを実行しないため、wkhtmltopdfが`--header-html`のURLにクエリ(`?page=1&topage=5`)を付けてJSで差し込んでいたページ変数は、プレースホルダの文字列置換で実現します。

| オプション | 方針 | 備考 |
|---|---|---|
| `--header-left` / `--header-center` / `--header-right` | ✅ 対応 | `@page`のmargin boxへマップ |
| `--footer-left` / `--footer-center` / `--footer-right` | ✅ 対応 | 同上 |
| `--header-html <url>` / `--footer-html <url>` | ✅ 対応 | 別のHTMLをレンダリングして余白へ合成する |
| `--header-line` / `--no-header-line` | ✅ 対応 | |
| `--footer-line` / `--no-footer-line` | ✅ 対応 | |
| `--header-spacing <real>` / `--footer-spacing <real>` | ✅ 対応 | mm |
| `--header-font-name` / `--header-font-size` | ✅ 対応 | |
| `--footer-font-name` / `--footer-font-size` | ✅ 対応 | |
| `--replace <name> <value>` | ✅ 対応 | ヘッダ/フッタ内の`[name]`を置換。sghtmltopdfのプレースホルダ方式と直接対応する |

wkhtmltopdfの組み込みプレースホルダのうち、使えるのは`[page]`・`[frompage]`・`[topage]`・`[date]`・`[time]`・`[title]`/`[doctitle]`です。
`[section]`/`[subsection]`(直近の見出し)と`[webpage]`/`[sitepage]`/`[sitepages]`(複数入力向け)は非対応です。
`--replace`で任意の名前を定義できます。

## TOC Options

生成される目次のHTML構造と既定スタイルは、wkhtmltopdfの既定TOC XSLの出力に合わせてあります。
階層は入れ子の`<ul>`、各項目は`<li><div><a>見出し</a><span>ページ番号</span></div></li>`です。

| オプション | 方針 | 備考 |
|---|---|---|
| `--toc-header-text <text>` | ✅ 対応 | 既定"Table of Contents"。`<h1>`のテキスト |
| `--toc-level-indentation <width>` | ✅ 対応 | 既定1em。`ul { padding-left }` |
| `--toc-text-size-shrink <real>` | ✅ 対応 | 既定0.8。`ul ul { font-size: 80% }` |
| `--disable-dotted-lines` | ✅ 対応 | `div`の`border-bottom: dashed`を出さない |
| `--disable-toc-links` | ✅ 対応 | 目次→見出しのリンク(`<a href>`)を出さない |
| `--xsl-style-sheet <file>` | ❌ 非対応 | XSLT非対応。見た目の変更は`--user-style-sheet`で行う |

## sghtmltopdf独自のオプション(wkhtmltopdfに無い)

| オプション | 内容 |
|---|---|
| `--font <path>` / `--font-index <N>` | フォントの明示指定(複数可、任意)。省略時はシステムフォントを使う |
| `--gothic-font` / `--mono-font` / `--serif-font`(+`-index`) | 汎用family名(`sans-serif`/`monospace`/`serif`)の実体指定 |
| `--allow-remote-assets` | http(s)絶対URLのフェッチ許可 |
| `--streaming` | [ストリーミングモード](../usage/cli/streaming.md)で処理する |
| `--base-url <url\|dir>` | stdin入力時などの相対解決の基準 |
| `--author` / `--subject` / `--keywords` | PDF Info辞書(wkhtmltopdfは`--title`のみ) |
| `--cover <path>` / `--toc` | 表紙・目次(wkhtmltopdfは位置引数) |
| `server` サブコマンド | HTTPサーバモード |
