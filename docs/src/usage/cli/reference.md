# CLIリファレンス

`sghtmltopdf`コマンドの全オプション。

```sh
sghtmltopdf [OPTIONS] <INPUT.HTML>
sghtmltopdf server [OPTIONS]
```

上が変換、下が[HTTPサーバ](../server.md)です。

wkhtmltopdfのオプションとの対応(非対応にしたものを含む全一覧)は[wkhtmltopdfオプション対応表](../../migration/wkhtmltopdf-options.md)を参照してください。

## 基本

```sh
# もっとも単純な使い方(フォントはシステムのものが使われる)
sghtmltopdf invoice.html -o invoice.pdf

# 出力先を省略すると入力の拡張子を .pdf にしたもの
sghtmltopdf invoice.html

# 標準入力から読み、標準出力へ書く
cat invoice.html | sghtmltopdf - -o - > invoice.pdf
```

## 入出力

| オプション | 既定 | 説明 |
|---|---|---|
| `<INPUT.HTML>` | (必須) | 入力HTML。`-`で標準入力 |
| `-o`, `--output <PATH>` | 入力の拡張子を`.pdf`に | 出力先。`-`で標準出力。標準入力から読む場合は省略できない |
| `--base-url <URL\|DIR>` | 入力HTMLのあるディレクトリ | 相対参照の解決基準。http(s)のURLを渡すと`<base href>`の既定値になる(HTML内の`<base href>`が優先) |
| `--encoding <NAME>` | 自動判定 | 入力の文字エンコーディング。判定順は BOM > `--encoding` > `<meta charset>` > UTF-8 |
| `--streaming` | オフ | [ストリーミングモード](streaming.md)で処理する |

出力ファイルは一時ファイルへ書いてから`rename`されるため、失敗したときに壊れたPDFが残ることはありません。

## ページ設定

| オプション | 既定 | 説明 |
|---|---|---|
| `-s`, `--page-size <SIZE>` | A4 | `A3`/`A4`/`A5`/`Letter`/`Legal`(大文字小文字を区別しない) |
| `--page-width <LENGTH>` | | 用紙の幅。`--page-size`より優先 |
| `--page-height <LENGTH>` | | 用紙の高さ。`--page-size`より優先 |
| `-O`, `--orientation <O>` | Portrait | `Landscape`は最後に幅と高さを入れ替える |
| `-T`, `--margin-top <LENGTH>` | 1in (96px) | 上マージン |
| `-B`, `--margin-bottom <LENGTH>` | 1in | 下マージン |
| `-L`, `--margin-left <LENGTH>` | 1in | 左マージン |
| `-R`, `--margin-right <LENGTH>` | 1in | 右マージン |

長さの単位は`mm`/`cm`/`in`/`pt`/`px`。
単位を省略するとmmです(wkhtmltopdf互換)。

> CSSの`@page`との関係
>
> これらのオプションは初期値であり、HTMLのCSSに`@page { size: … }`や`@page { margin: … }`が書かれていればそちらが勝ちます(プロパティ単位)。
> wkhtmltopdfとは逆なので注意してください。

## フォント

| オプション | 説明 |
|---|---|
| `--font <PATH>` | 使うフォント(複数指定可)。省略するとシステムフォントを使う |
| `--font-index <N>` | 直前の`--font`に対する、TrueType Collection(`.ttc`)内のフェイス番号 |
| `--gothic-font <PATH>` (+`--gothic-font-index`) | `font-family: sans-serif`の実体 |
| `--serif-font <PATH>` (+`--serif-font-index`) | `font-family: serif`の実体 |
| `--mono-font <PATH>` (+`--mono-font-index`) | `font-family: monospace`の実体 |

フォントの解決順は「`--font` → `@font-face` → `font-family`名でのシステム探索」。
それでも1つも見つからない場合だけ、システムの`sans-serif`候補が既定フォントになります。

> `--font`を指定しないと出力が実行環境のフォントに依存します。
> サーバ運用やCIで出力を安定させたい場合は`--font`(または`@font-face`)を明示してください。
> 詳しくは[フォント](../../supports/fonts.md)を参照。

## PDFの出力形式・メタデータ

| オプション | 既定 | 説明 |
|---|---|---|
| `--title <TEXT>` | HTMLの`<title>` | PDFの`/Title` |
| `--author` / `--subject` / `--keywords <TEXT>` | | Info辞書の各項目(sghtmltopdf独自) |
| `-d`, `--dpi <DPI>` | 96 | CSS pxを何dpiとして解釈するか。`72`にすると1px=1pt |
| `--zoom <FACTOR>` | 1.0 | 拡大率。`--dpi`の係数に掛かる |
| `-g`, `--grayscale` | オフ | 塗り・線をグレースケール化(sRGB相対輝度) |
| `--no-pdf-compression` | オフ | PDFオブジェクトのFlate圧縮を止める(画像データは対象外) |

`/Producer`と`/CreationDate`は常に書かれます。

> グレースケールの限界
>
> JPEG(`/DCTDecode`のパススルー)とCMYK画像はデコーダを持たないため、変換されずカラーのまま残ります。

## コンテンツの挙動

| オプション | 説明 |
|---|---|
| `--no-images` | `<img>`とCSS`background-image`を読み込まない |
| `--no-background` | 要素の背景(色・画像)を描かない |
| `--user-style-sheet <PATH>` | ユーザーオリジンのCSS(複数指定可)。UAスタイルより強く、著者CSSより弱い |
| `--minimum-font-size <PX>` | 算出`font-size`の下限 |
| `--disable-external-links` | 外部リンク(http(s))のPDF注釈を作らない |
| `--disable-internal-links` | 内部リンク(`#id`)のPDF注釈を作らない |
| `--keep-relative-links` | 相対URLの外部リンクを絶対化せずそのまま書く |
| `--load-media-error-handling <ignore\|abort>` | 画像・CSS・フォントの取得失敗時の挙動(既定`ignore`) |

## ヘッダー/フッター

2つの方法があります。
同じ側に両方を指定した場合は`--header-html`が優先されます。

### 1. テキストで指定する

`@page`のmargin boxへマップされます。

```
sghtmltopdf report.html \
  --header-center "四半期レポート" \
  --footer-right "[page] / [topage]" \
  --header-line
```

| オプション | 説明 |
|---|---|
| `--header-left` / `--header-center` / `--header-right <TEXT>` | ヘッダーの3分割位置 |
| `--footer-left` / `--footer-center` / `--footer-right <TEXT>` | フッターの3分割位置 |
| `--header-font-name` / `--header-font-size` | ヘッダーのフォント(footerも同様) |
| `--header-line` / `--footer-line` | 罫線を引く |
| `--header-spacing` / `--footer-spacing <MM>` | 本文との間隔。その分だけマージンが増える |
| `--default-header` | タイトルとページ番号の既定ヘッダー |
| `--replace <NAME=VALUE>` | 任意の`[NAME]`を値へ置換(複数指定可) |

プレースホルダは`[page]`(現在ページ)・`[topage]`(総ページ数)・`[frompage]`・`[title]`/`[doctitle]`・`[date]`・`[time]`、および`--replace`で定義した名前です。

`[section]`/`[subsection]`と`[webpage]`/`[sitepage]`/`[sitepages]`は非対応です。

### 2. HTMLで指定する

```sh
sghtmltopdf report.html --header-html header.html --footer-html footer.html
```

各ページの余白領域へ、別のHTMLをレンダリングして合成します。
プレースホルダはHTMLのテキストとして置換されます(JavaScriptは実行しません)。

* 余白に入りきらない分はクリップされます(マージンは自動で広がりません)
* 外部リソースを取得しません。使えるのはインラインの`<style>`・テキスト・枠線・背景色までで、`<img>`と外部CSSは非対応です
* ヘッダー/フッターHTML内の`@font-face`は読み込みません。そこでしか使わないフォントは`--font`で明示してください

## 表紙と目次

```sh
sghtmltopdf report.html --cover cover.html --toc --footer-center "[page]"
```

書き出される順は 表紙 → 目次 → 本文 です。

| オプション | 既定 | 説明 |
|---|---|---|
| `--cover <PATH>` | | 表紙にするHTML。ページ番号に数えず、ヘッダー/フッターも出さない |
| `--toc` | オフ | 目次を本文の前に挿入する(ストリーミングモードでは使えない) |
| `--toc-header-text <TEXT>` | `Table of Contents` | 目次の`<h1>` |
| `--toc-level-indentation <WIDTH>` | `1em` | 階層1段ごとのインデント |
| `--toc-text-size-shrink <REAL>` | `0.8` | 階層1段ごとの文字サイズ比 |
| `--disable-dotted-lines` | (引く) | 項目の破線の下線を引かない |
| `--disable-toc-links` | (張る) | 目次から見出しへのリンクを張らない |
| `--enable-toc-back-links` | (張らない) | 見出しから目次へ戻るリンクを張る |
| `--page-offset <N>` | 0 | ページ番号の起点をずらす |

目次のHTML構造と既定スタイルはwkhtmltopdfの既定TOC XSLの出力に合わせてあります(階層は入れ子の`<ul>`、各項目は`<div><a>見出し</a><span>ページ番号</span></div>`)。
見た目を変えたい場合は`--user-style-sheet`でCSSを当ててください(XSLTは非対応)。

見出しは`h1`〜`h6`から集めます。
`id`が無い見出しには自動で宛先名が振られます。

## アクセス制御

| オプション | CLIの既定 | サーバの既定 |
|---|---|---|
| `--enable-local-file-access` / `--disable-local-file-access` | 許可 | 禁止 |
| `--allow <PATH>` | 制限なし | 制限なし |
| `--allow-remote-assets` | 禁止 | 禁止 |

`--allow`を1つ以上指定すると、ローカル参照はそのディレクトリ配下だけに限定されます。
`<img src>`・外部CSS・`@font-face`のすべてに効きます。

判定は実パス(シンボリックリンクを辿った後のパス)で行います。
指定したディレクトリが存在しない場合は起動時にエラーになります。
黙って無視すると、許可した範囲と実際に効く範囲がずれるためです。

### 基準ディレクトリの外への参照

ローカル参照は既定で基準ディレクトリ(`--base-url`、既定は入力HTMLのあるディレクトリ)の中に閉じます。
`../`で外へ出ようとする参照はエラーになります。
信頼できないHTMLを変換したときに`<img src="../../../../etc/passwd">`のような参照で任意のファイルを読み出されるのを防ぐためです。

`assets/../images/logo.png`のように基準ディレクトリの中で完結する`../`は従来どおり使えます。

外のファイルを意図的に参照する場合は`--allow`で範囲を明示してください。
`--allow`を指定した場合は、基準ディレクトリではなく許可したディレクトリが境界になります。

```console
$ sghtmltopdf pages/index.html -o out.pdf
エラー: ../images/logo.png: 基準ディレクトリ(pages)の外を参照しています。
  外部のファイルを読む場合は --allow でディレクトリを明示してください

$ sghtmltopdf pages/index.html --allow . -o out.pdf
```

判定はパス文字列に対して行うため、基準ディレクトリ配下のシンボリックリンクは辿ります。
シンボリックリンクの先まで含めて閉じたい場合は`--allow`を使ってください(こちらは実パスで判定します)。

### `/`で始まる参照

`/assets/logo.png`のように`/`で始まる参照は、まず基準ディレクトリからのサイトルート相対(`<基準ディレクトリ>/assets/logo.png`)として解決します。
Railsのアセットパイプラインが出すパスがこの形なので、precompile済みの`public/`を基準ディレクトリにすればそのまま解決できます。

そこにファイルが無い場合に限り、同じ文字列をファイルシステムの絶対パスとして解釈し直します。
`<img src="/var/www/app/public/logo.png">`のような書き方のためのフォールバックです。
このとき読めるかどうかは他の参照と同じ規則で決まります。
絶対パスが基準ディレクトリの中を指していればそのまま読め、外を指していれば`--allow`が要ります。

どちらの解釈でもファイルが見つからない場合は、両方のパスを挙げたエラーになります。

## 入力の大きさの制限

要素数がおよそ50万ノードを超えるHTMLはエラーになります。
算出スタイル・ボックスツリー・レイアウト結果がノード数に比例して積み上がるためで、実測では1ノードあたり472B〜1210Bでした。

数千ページ規模の文書でも数十万ノードなので、実在の文書がここに当たることはまずありません。
当たった場合は文書を分割するか、[ストリーミングモード](streaming.md)を使ってください。
ストリーミングモードでは処理済みの部分が随時解放されるため、総量が上限を超えていても変換できます。

テキストの量に比例するメモリはこの上限では抑えられません(要素3個でも10MiBのテキストなら約1.7GiB使います)。
HTTPサーバモードでは`--max-body-size`がその役割を担います。

## ログとexit code

`--log-level <none|error|warn|info>`(既定`info`)、`-q`/`--quiet`(=`--log-level none`)。

| code | 意味 |
|---|---|
| 0 | 成功 |
| 1 | 使用法エラー(不明なオプション、値の形式不正、非対応オプションの指定) |
| 2 | 入力/リソースエラー(ファイルが無い、フォントが読めない、`abort`指定での取得失敗) |
| 3 | レンダリングエラー(ストリーミングモードの制約違反など) |
| 4 | 制限時間超過(HTTPサーバモードの`--timeout`のみ。CLIには時間制限がないため出ません) |
