# フォント

PDFは文書の中にフォントを埋め込みます。
ブラウザと違って「見る人の環境にあるフォントで表示する」ということができないため、どのフォントを使うかは変換時に決まります*

## フォントが決まる順番

1. CLIの`--font`(および`--gothic-font`/`--serif-font`/`--mono-font`)
2. CSSの`@font-face`
3. `font-family`に書かれた名前でのシステムフォント探索
4. 文書中の文字を描画できるフォントのシステム探索

どれでも1つも見つからなかった場合だけ、システムの`sans-serif`候補が既定フォントになります。

4番目は、`font-family`をどこにも書いていない日本語文書のように、名前が手掛かりにならない場合の網です。
1〜3で集めたフォントで描画できない文字が文書に含まれていれば、その文字を持つシステムフォント(日本語ならNoto Sans CJK JPなど)を探して追加します。
この探索はウェイト・スタイルごとに行うので、太字と通常が混在する文書では両方の面が追加されます(通常の文字が太字の面で描かれてしまうのを防ぐためです)。
それでも描画できない文字が残る場合は、豆腐(□)になる前に警告を出します。

```
警告: 文字 "ไ" を描画できるフォントがありません(豆腐になります)。
  --font/--gothic-font か @font-face でフォントを明示してください
```

> サーバやCIでは`--font`を明示してください。
> 指定しないと出力が実行環境のフォント構成に依存します。
> 同じHTMLが開発機と本番で違う見た目になる、という事故はここから起きます。

## 汎用ファミリー名

`serif` / `sans-serif` / `monospace`はシステムフォントから解決されます(`cursive` / `fantasy`は解決しません)。

日本語では、この解決を環境任せにすると本文の書体が変わってしまうので、CLIから決定的に指定できます。

```sh
sghtmltopdf invoice.html \
  --gothic-font NotoSansJP-Regular.ttf \   # font-family: sans-serif の実体
  --serif-font  NotoSerifJP-Regular.ttf \  # font-family: serif の実体
  --mono-font   NotoSansMono-Regular.ttf   # font-family: monospace の実体
```

TrueType Collection(`.ttc`)を使う場合は、直前の`--font`系オプションに対して`--font-index`でフェイス番号を指定します。

## `@font-face`

```css
@font-face {
  font-family: "MyFont";
  src: url("fonts/MyFont-Regular.ttf");
  font-weight: 400;
  font-style: normal;
}

body { font-family: "MyFont", sans-serif; }
```

対応するディスクリプタは`font-family` / `src` / `unicode-range` / `font-weight` / `font-style`です。
`src`の`local()`と、`format()`/`tech()`付きの`url()`も受け付けます。
`font-display`などその他のディスクリプタは無視されます。

> フォントファイルはTTF/OTFのみです。WOFF/WOFF2は非対応なので、Webで配信しているwebfontをそのまま指すとエラーになります。
> 元のTTF/OTFを使ってください。

### `src: url()`に書けるもの

ローカルの相対パス・絶対パスのほかに、`data:`URI(base64)と`http(s)`のURLを書けます。
参照の解決とアクセス制御は`<img src>`や外部CSSとまったく同じ規則で、`<base href>`・`--base-url`・`--allow`・`--allow-remote-assets`がそのまま効きます。

```css
@font-face {
  font-family: "Embedded";
  src: url(data:font/ttf;base64,AAEAAAAM…);
}
```

`data:`URIはフォントをHTMLの中で自己完結させられるので、文字列を直接渡す変換やHTTPサーバ経由の変換のように、変換する側のファイルシステムを当てにできない構成で使えます。

読み込みの待ち合わせはありません。
headless Chromeで必要だった`document.fonts.ready`待ちのような処理は不要で、フォントが未解決のままPDF化されることはありません。

## `unicode-range`

文字の範囲ごとにフォントを切り替えられます。
英数字は欧文フォント、日本語は和文フォント、という典型的な構成がそのまま書けます。

```css
@font-face {
  font-family: "Mixed";
  src: url("fonts/Latin.ttf");
  unicode-range: U+0-24F, U+1E00-1EFF;
}
@font-face {
  font-family: "Mixed";
  src: url("fonts/JP.ttf");            /* 上の範囲外はこちら */
}
```

* 単一コードポイント・範囲・ワイルドカード(`U+4??`)・カンマ区切りの複数指定に対応します
* 宣言された範囲はハードフィルタとして働きます。範囲外の文字には、そのフォントが実際にグリフを持っていても使いません
* `unicode-range`を書かなかったフォント(`local()`・`--font`・システム探索を含む)は全域をカバーします
* 範囲が重なった場合は、CSSの中で先に宣言されたほうが優先されます

## 太字と斜体

| 指定 | 挙動 |
|---|---|
| `font-weight` | `normal`/`bold`/`100`〜`900`。数値は600以上を`bold`とみなす2値化。太字のフォントが無い場合は、塗りに縁取りを足した疑似ボールドで描画します |
| `font-style` | `normal`/`italic`/`oblique`(`oblique`は`italic`と同一視)。イタリック字形が無い場合は、テキスト行列のせん断による疑似イタリックになります |

`font`ショートハンドは非対応です。
`font-size`・`font-family`などのロングハンドを個別に書いてください。

## 絵文字

カラー絵文字はそのままカラーで描画します。
対応しているのは埋め込みビットマップ(`CBDT`/`CBLC`・`sbix`)と`COLR`/`CPAL`のversion 0です。

```sh
sghtmltopdf report.html --font NotoColorEmoji.ttf
```

macOSのApple Color Emoji(`sbix`)、LinuxのNoto Color Emoji(`CBDT`/`CBLC`)のどちらも使えます。
明示しなくても、文書に出てくる文字を描けるフォントとしてシステムから自動的に見つかります。

モノクロのアウトラインフォント(GoogleのNoto Emojiなど)もこれまでどおり使えます。

### PDFの中でどうなるか

カラーグリフはType 3フォントとしてPDFへ書き出します。
グリフはテキストのままなので、抽出・検索・コピーは通常の文字と同じように効きます。

元のフォントプログラムは埋め込みません。
輪郭を持たないフォントはサブセット化しても削るものが無いため、埋め込むと10MB超のファイルがほぼ素通しでPDFへ入ってしまいます。
かわりにビットマップは画像として、`COLR`のレイヤはパスの塗りとして、使ったグリフの分だけが入ります。

`--grayscale`は絵文字のビットマップにも効きます。

### 対応していないもの

* COLRv1(グラデーション・変換・合成)。COLRv1のフォントは`glyf`を持つため、ベースの輪郭がモノクロで描画されます
* OpenType SVG(`SVG `テーブル)
* `font-palette`によるパレットの選択(常に0番のパレットを使います)

## サブセット化

埋め込まれるのは実際に使ったグリフだけです。
日本語フォントを丸ごと指定しても、PDFのサイズは文書に出てくる文字の分にしかなりません。

## ストリーミングモードでの注意

[ストリーミングモード](../usage/cli/streaming.md)では、文書全体を一度に持たないため
上の3・4のシステムフォント探索が行われません(警告を出して既定フォントで描画します)。
`--font`系オプションか`@font-face`で明示すれば、ストリーミングでも意図どおりのフォントになります。

例外として、フォントを1つも指定しなかった場合だけは、既定フォント(ラテン)に加えてCJKを描画できるフォントを1本先回りで読み込みます。
日本語の文書を何も指定せずストリーミングで変換しても豆腐にならないのはこのためです。
CJK以外のスクリプトは警告の対象になります。
