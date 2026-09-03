# wicked_pdfからの移行ガイド

[wicked_pdf](https://github.com/mileszs/wicked_pdf)(+ wkhtmltopdf)を使っているRailsアプリを、sghtmltopdfのgemへ移すための対応表と注意点。

wkhtmltopdfのオプションの対応状況は[wkhtmltopdfオプション対応表](wkhtmltopdf-options.md)と[wkhtmltopdfからの移行](wkhtmltopdf.md)を参照してください。
ここでは「Rails/Rubyから見た違い」だけを扱います。

## 最小の置き換え

```ruby
# Gemfile
- gem "wicked_pdf"
- gem "wkhtmltopdf-binary"
+ gem "sghtmltopdf"
```

コントローラはそのまま動くはずです。

```ruby
def show
  respond_to do |format|
    format.pdf { render pdf: "invoice", template: "invoices/show", layout: "pdf" }
  end
end
```

外部プロセスの起動が無くなるので、`wicked_pdf`の`exe_path`(wkhtmltopdfのバイナリの場所)の設定は不要になる。

## 設定の置き場所

```ruby
# config/initializers/sghtmltopdf.rb
Sghtmltopdf.configure do |c|
  c.page_size   = "A4"
  c.margin_top  = "20mm"
  c.gothic_font = Rails.root.join("vendor/fonts/NotoSansJP-Regular.ttf")
end
```

wicked_pdfの`WickedPdf.config = {...}`に相当する。
マージ順は
グローバル設定 → `render`の引数で、後者が勝ちます。

## オプション名の対応

wicked_pdfはネストしたHash(`margin: {top: 10}`)を使うが、sghtmltopdfは
CLIのフラグ名をそのままキーにした平坦なHashを使う(`_`が`-`に対応する。`page_size:` → `--page-size`)。
オプションの定義はRust側の1箇所に集約されていて、Ruby側はホワイトリストを持ちません。

| wicked_pdf | sghtmltopdf | 備考 |
|---|---|---|
| `pdf: "name"` | 同じ | ファイル名(`.pdf`は自動で付く) |
| `template:` / `layout:` / `locals:` / `formats:` | 同じ | Railsのビュー描画へそのまま渡る |
| `disposition:` / `filename:` / `status:` | 同じ | 既定の`disposition`は`inline` |
| `show_as_html: true` | 同じ | PDFにせずHTMLを返すデバッグ用 |
| `page_size: "A4"` | `page_size: "A4"` | |
| `page_height:` / `page_width:` | 同じ | 単位付きの文字列(`"210mm"`)で渡す |
| `orientation: "Landscape"` | 同じ | |
| `margin: {top: 10, bottom: 10}` | `margin_top: "10mm"`, `margin_bottom: "10mm"` | wicked_pdfの数値はmm。単位を明示する |
| `dpi:` / `zoom:` | 同じ | |
| `grayscale: true` | 同じ | |
| `background: false` | `no_background: true` | |
| `encoding: "UTF-8"` | 同じ | |
| `title:` | 同じ | PDFのメタデータ |
| `user_style_sheet:` | 同じ | パスの配列も可 |
| `no_pdf_compression: true` | 同じ | |
| `cover: "shared/cover"` | `cover: <ファイルパス>` | テンプレート名ではなくHTMLファイルのパス(後述) |
| `toc: {}` | `toc: true` | 見た目は`toc_header_text:`などで調整 |
| `header: {left:, center:, right:}` | `header_left:` / `header_center:` / `header_right:` | |
| `header: {html: {template: "..."}}` | `header_html: <ファイルパス>` | 同上 |
| `header: {line: true, spacing: 5, font_name:, font_size:}` | `header_line: true`, `header_spacing: 5`, `header_font_name:`, `header_font_size:` | footerも同様 |
| `outline: {}` | — | PDFアウトラインは非対応 |
| `disable_javascript` / `javascript_delay` / `window_status` | — | JSは実行しない(設計上の非目標) |
| `print_media_type` | — | 常に`print`メディア扱い |
| `lowquality` / `viewport_size` / `disable_smart_shrinking` | — | WebKit固有 |
| `exe_path` / `wkhtmltopdf` | — | 外部プロセスを使わない |
| `extra` | — | 生のコマンドライン文字列は受けない。個別のキーで指定する |

対応していないキーを渡すと、レンダリング時に`Sghtmltopdf::UsageError`が理由付きで上がる(黙って無視はしない)。

### 表紙・ヘッダー・フッターのHTML

wicked_pdfはRailsのテンプレート名を受け取って内部で描画するが、sghtmltopdfの`--cover`/`--header-html`/`--footer-html`はファイルのパスを取ります(CLIと同じ経路に合流させるため)。
Railsのテンプレートを使いたい場合は、自分で描画して一時ファイルへ書き出してください。

```ruby
def show
  header = Tempfile.new(["header", ".html"])
  header.write(render_to_string(template: "invoices/header", layout: false))
  header.flush

  render pdf: "invoice", template: "invoices/show", header_html: header.path
ensure
  header&.close!
end
```

## ビューヘルパ

| wicked_pdf | sghtmltopdf |
|---|---|
| `wicked_pdf_stylesheet_link_tag` | `sghtmltopdf_stylesheet_link_tag` |
| `wicked_pdf_image_tag` | `sghtmltopdf_image_tag` |
| `wicked_pdf_asset_path` | `sghtmltopdf_asset_path`(見つからなければ`nil`) |
| `wicked_pdf_javascript_include_tag` | — (JSを実行しないので不要) |
| `wicked_pdf_asset_base64` | — (`sghtmltopdf_image_tag`が既定で埋め込むので不要) |

素の`stylesheet_link_tag`/`image_tag`も、アセットが`public/`配下へprecompileされていればそのまま動きます。
PDFのレンダリングはHTTPサーバを介さないので、`/assets/…`のようなURLは`--base-url`(Railsでの既定は`Rails.root/public`)を基準にローカルファイルとして解決される。

開発環境のようにアセットがまだ`public/`に無い場合は、アセットの中身を文書へ埋め込むヘルパを使う。
`sghtmltopdf_stylesheet_link_tag`はCSSを`<style>`へ展開し、`sghtmltopdf_image_tag`は画像を`data:`URIとして埋め込む。
`wicked_pdf_image_tag`が`file://`のURLを出していたのに対し、こちらは取得そのものが起きない。

```erb
<%= sghtmltopdf_stylesheet_link_tag "pdf" %>
<%= sghtmltopdf_image_tag "logo.png" %>
```

## 既定値の違い

* マージン: wkhtmltopdfは四辺10mm。sghtmltopdfは四辺1in(96px)。
  同じ見た目にしたければ`margin_*`を明示する
* CLIオプションとCSSの`@page`: wkhtmltopdfはCLIが勝つが、sghtmltopdfは `@page` が勝つ(オプションは初期値)
* ローカルファイルの参照範囲: Railsでは`--allow`の既定が`Rails.root`になる。アプリの外(例: `/usr/share/fonts`)のファイルを`<img>`や`@font-face`の`url()`で参照している場合は、`Sghtmltopdf.configure { |c| c.allow = [Rails.root.to_s, "/usr/share/fonts"] }`のように明示する。`--font`系(`gothic_font`など)で渡すフォントはこの制限を受けない
* リモート取得: `http(s)`のアセット取得は既定で無効。必要なら`allow_remote_assets: true`
* `disable_local_file_access`との併用: `--allow`は読み取り範囲を狭めるだけで、許可を与えるものではない。wicked_pdfで`disable_local_file_access: true`と`allow: [dir]`を併用して「dirだけ読める」状態にしていた場合、そのまま持ち込むとローカル読み取りが全て止まる。`allow`だけを残すこと

## フォント

wkhtmltopdfはシステムのフォント設定に依存するが、sghtmltopdfは`gothic_font`/`serif_font`/`mono_font`で指定できる。
日本語を出す場合は、コンテナのフォント事情に左右されないよう明示するのが安全。

```ruby
Sghtmltopdf.configure do |c|
  c.gothic_font = Rails.root.join("vendor/fonts/NotoSansJP-Regular.ttf")
end
```

## 別プロセスへ逃がす(wicked_pdfには無い選択肢)

wicked_pdfはリクエストごとにwkhtmltopdfのプロセスを起動するが、sghtmltopdfのgemはアプリのプロセス内で変換する(重い処理の間はGVLを解放するのでPumaの他スレッドは止まらない)。
それでもアプリのCPUを使いたくない場合は、`server_url`で別プロセス(`sghtmltopdf server`)へ委譲できる。

```ruby
Sghtmltopdf.configure { |c| c.server_url = "http://pdf.internal:8080" }
```

負荷分散はLB(nginx・k8s Service)を前段に置く前提で、URLは1つだけ受ける。
到達できないときは`Sghtmltopdf::ServerError`になり、ローカル変換へはフォールバックしない。

サーバモードでは`base_url`・`allow`・フォント指定などローカルパスを取るオプションはリクエストから指定できない(サーバ起動時にだけ設定できる)。
Railtieが入れる既定値は自動的に外れるが、`configure`で明示的に設定している場合は400(`UsageError`)になるので、サーバ側の起動オプションへ移す。

## まだ無いもの

* PDFの結合・アウトライン: 非対応

なお逐次出力(ストリーミング)はwicked_pdfには無い機能で、ブロック付きの`render`で使える。
`ActionController::Live`と組み合わせれば、確定したページから順にレスポンスへ流せます → [Ruby / Rails](../usage/ruby_rails.md#railsで逐次返却する)
