# Ruby / Rails

gem `sghtmltopdf`の使い方です。
エンジン本体はRust製で、ネイティブ拡張(magnus + rb-sys)経由で同じプロセスの中で動きます。
外部プロセスの起動も一時ファイルの受け渡しもありません。

変換オプションはCLIとまったく同じものが使えるので、オプションの意味は[CLIリファレンス](../cli/reference.md)を参照してください。
このページはRuby側の作法(命名規則・Rails連携・エラー・サーバ委譲)を扱います。

wicked_pdfから移ってくる場合は[wicked_pdfからの移行](../migration/wicked-pdf.md)もあわせて読んでください。

## インストール

```ruby
# Gemfile
gem "sghtmltopdf"
```

ビルド済み(precompiled)のgemを配布するため、Rustのツールチェインは不要です。

| | 対応 |
|---|---|
| プラットフォーム | `x86_64-linux` / `aarch64-linux` / `x86_64-linux-musl` / `aarch64-linux-musl` / `arm64-darwin` |
| Ruby | 3.2以上 |

Linuxはglibc(Debian/Ubuntu系)とmusl(Alpine)の両方があり、`gem install`が環境に合うほうを選びます。
Windows・Intel Macは対象外で、これらの環境ではインストールできません。
[サーバへ委譲する](#サーバへ委譲する)という手があります。

## 基本

```ruby
pdf = Sghtmltopdf.render("<h1>請求書</h1>")             # → PDFのバイト列(String)
Sghtmltopdf.render_to_file(html, "invoice.pdf")         # → ファイルへ書き出す(nil)
```

* 返り値のエンコーディングはASCII-8BIT(バイナリ)です
* 入力のHTMLはバイト列としてそのまま渡ります。文字コードの判定はエンジン側で
  BOM > `encoding:` > `<meta charset>` > UTF-8の順に行われるので、UTF-8のStringならそのまま渡せます。
Shift_JISなどを渡す場合は`encoding: "Shift_JIS"`を明示してください
* `render_to_file`は一時ファイルへ書いてからrenameするので、途中で失敗しても壊れたPDFが残りません
* 重い処理(レイアウト・PDFエンコード)の間はGVLを解放するので、Pumaの他のスレッドは止まりません。複数スレッドから同時に呼べます

## オプション

CLIのロングオプションから`--`を取り、`-`を`_`にした名前をキーにします。
値の解釈もCLIと同一です(同じパーサへ通しているため)。

```ruby
Sghtmltopdf.render(html, page_size: "A4", margin_top: "20mm", toc: true)
#                        --page-size A4   --margin-top 20mm   --toc
```

| 値の書き方 | 意味 |
|---|---|
| `page_size: "A4"` | 値を取るオプション |
| `grayscale: true` | 値を取らないフラグ |
| `grayscale: false` / `nil` | 指定なしと同じ |
| `allow: ["/a", "/b"]` | 同じオプションの繰り返し |
| `font: {path: "a.ttc", index: 1}` | `--font a.ttc --font-index 1`(順序も保つ) |

キー名の妥当性はRuby側では検査しません。
オプションの定義をRust側の1か所に集約しているため、未知のキーはエンジン側が`UsageError`として報告します。

wicked_pdfのような入れ子のHash(`margin: {top: 10}`)は受け付けません。
wicked_pdf/wkhtmltopdfの数値はmm、こちらのCLIはpx解釈なので、機械的に平坦化すると黙って別の余白になるためです。
`margin_top: "10mm"`と書いてください。

### Ruby側だけのオプション

CLIには無い、gemが解釈するキーです。

| キー | 既定 | 説明 |
|---|---|---|
| `server_url` | なし | 指定するとHTTPサーバモードへ委譲する |
| `server_open_timeout` | 5 | 接続のタイムアウト(秒) |
| `server_read_timeout` | 120 | 応答のタイムアウト(秒) |
| `chunk_size` | 65536 | ブロック付き`render`で1回に渡すバイト数の目安(ローカル変換のみ) |

Railsのレンダラ(`render pdf:`)では、これに加えて`disposition`・`filename`・`status`・`show_as_html`を解釈します。

## グローバル設定

```ruby
# config/initializers/sghtmltopdf.rb など
Sghtmltopdf.configure do |c|
  c.page_size   = "A4"
  c.gothic_font = Rails.root.join("vendor/fonts/NotoSansJP-Regular.ttf")
end
```

マージ順はグローバル設定 → 呼び出し時の引数で、後者が勝ちます。
`Sghtmltopdf.reset_config!`で空に戻せます(主にテスト用)。

## フォント

指定しなければシステムのフォントが使われるため、出力が実行環境に依存します。
コンテナのフォント事情に左右されたくない場合は明示してください。

```ruby
Sghtmltopdf.configure do |c|
  c.gothic_font = "/app/vendor/fonts/NotoSansJP-Regular.ttf"  # sans-serif
  c.serif_font  = "/app/vendor/fonts/NotoSerifJP-Regular.ttf" # serif
  c.mono_font   = "/app/vendor/fonts/NotoSansMono-Regular.ttf"
end
```

`font`系で渡すフォントは、後述の`allow`(ローカル参照の制限)の対象外です。

## エラー

すべて`Sghtmltopdf::Error < StandardError`を継承します。
メッセージはCLIと同じ文言です。

| クラス | 起きるとき |
|---|---|
| `Sghtmltopdf::UsageError` | オプションの誤り(未知のキー、値の形式、非対応オプション) |
| `Sghtmltopdf::InputError` | 入力や出力ファイルの読み書きに失敗した |
| `Sghtmltopdf::RenderError` | レンダリングに失敗した |
| `Sghtmltopdf::TimeoutError` | 制限時間を超えて打ち切られた |
| `Sghtmltopdf::InternalError` | エンジン内部の想定外の失敗(バグ) |
| `Sghtmltopdf::ServerError` | サーバへ委譲したときの到達不能・過負荷 |

`InternalError`はネイティブ拡張の中でRustがパニックしたときに上がります。
拡張側で捕まえて通常の例外へ変換しているため、他のエラーと同じように`rescue`でき、ワーカープロセスは動き続けます。
これが出た場合はエンジンの不具合なので、再現するHTMLを添えて報告してください。

画像やCSSの取得失敗は既定では無視され、警告を出して続行します(`load_media_error_handling: "abort"`で中断できます)。

## Railsで使う

Railsが読み込まれているときだけRailtieが読み込まれるので、素のRuby・Sinatraでの利用には影響しません。

### レンダラ

```ruby
class InvoicesController < ApplicationController
  def show
    render pdf: "invoice",             # ファイル名(.pdfは自動で付く)
      template: "invoices/show",
      layout: "pdf",
      page_size: "A4", margin_top: "20mm"
  end
end
```

オプションは3つに振り分けられます。

| 種類 | キー |
|---|---|
| ビューの描画へ渡す | `template` `partial` `inline` `file` `plain` `html` `body` `layout` `locals` `formats` `variants` `handlers` `prefixes` `object` `collection` `assigns` `action` |
| レスポンスの組み立て | `disposition`(既定`inline`) `filename` `status` |
| デバッグ | `show_as_html`(PDFにせずHTMLを返す) |
| 上記以外すべて | 変換オプション |

`pdf:`の値が空ならアクション名がファイル名になります。
`filename:`があればそちらが勝ち、`.pdf`は二重に付きません。

### アセットのパス解決

PDFのレンダリングはHTTPサーバを介さないため、`/assets/…`のようなURLは
ローカルファイルとして解決されます。
Railtieが次の既定値を入れます。

| キー | 既定 | 意味 |
|---|---|---|
| `base_url` | `Rails.root/public` | 絶対パス参照の基準。precompile済みなら素の`stylesheet_link_tag`がそのまま動く |
| `allow` | `[Rails.root]` | ローカル参照をアプリ配下に限定する |

どちらも`Sghtmltopdf.configure`で上書きできます(イニシャライザの実行順に依存しません)。
`allow`の既定はテンプレートにユーザー入力が混ざっても文書外のファイルを読ませないためのものなので、アプリの外(例: `/usr/share/fonts`)を参照している場合は明示的に足してください。

開発環境のようにアセットがまだ`public/`へ書き出されていない場合のために、アセットの中身を文書へ埋め込むヘルパがあります。

```erb
<%= sghtmltopdf_stylesheet_link_tag "pdf" %>
<%= sghtmltopdf_image_tag "logo.png" %>
<%= sghtmltopdf_asset_path "logo.png" %>   <%# 見つからなければnil %>
```

`sghtmltopdf_stylesheet_link_tag`はCSSの中身を`<style>`へ展開します。
`sghtmltopdf_image_tag`は画像を`data:`URIとして`<img>`へ埋め込みます。
どちらも取得を伴わないため、`base_url`や`allow`の設定に依存せず、`public/`の外にあるファイルでも読めます。
画像が多くbase64でHTMLが膨らむ場合は、`sghtmltopdf_image_tag "logo.png", inline: false`で`base_url`基準の相対パスを出せます。
このとき`base_url`の外にあるファイルは、相対パスで指せないため埋め込みに戻ります。

## サーバへ委譲する

`server_url`を指定すると、変換を[HTTPサーバモード](../server/index.md)で動く
別プロセスへ投げます。
アプリのCPUを使いたくない場合や、gemの対応プラットフォーム外(Windowsなど)で動かす場合に使います。

```ruby
Sghtmltopdf.configure do |c|
  c.server_url = "http://pdf.internal:8080"
end

pdf = Sghtmltopdf.render(html, page_size: "A4")   # 委譲される
```

* URLは1つだけです。負荷分散はLB(nginx・k8s Service)を前段に置く前提です
* 到達できないときはローカルへフォールバックしません(`ServerError`)。
  サーバ起動時にだけ指定できるフォントが効かず、出力が変わってしまうためです
* HTTPのステータスは上のエラー分類へ対応します(400→`UsageError`、413→`InputError`、500→`RenderError`、その他→`ServerError`)

### サーバでは指定できないオプション

ローカルパスを取るものと出力先・アクセス制御はサーバ起動時にしか設定できません(指定すると`UsageError`)。

```
font, font-index, gothic-font, gothic-font-index, serif-font,
serif-font-index, mono-font, mono-font-index,
output, cover, header-html, footer-html, user-style-sheet, base-url,
allow, enable-local-file-access, disable-local-file-access,
allow-remote-assets, log-level, quiet
```

Railtieが入れる`base_url`/`allow`の既定値は自動的に外れるので、Railsでそのまま`server_url`を足しても400にはなりません。
`configure`で明示的に設定している場合は、サーバ側の起動オプションへ移してください。

## チャンクごとに受け取る

ブロックを渡すと、PDF全体を組み立ててから返す代わりにチャンクごとにブロックが呼ばれます(返り値は`nil`)。
Rackの`response.stream`へ流したり、S3のマルチパートアップロードへ繋いだりするための口です。

```ruby
Sghtmltopdf.render(html) { |bytes| response.stream.write(bytes) }
```

ローカル変換でもサーバ委譲でも、PDF全体が組み上がるのを待たずに書き出せます。
ローカルは確定したページから順に、サーバはその`?stream=1`(chunked transfer encoding)をそのまま流します。

ただし逐次になるのはPDFの書き出しだけで、HTMLのパースとレイアウトは文書全体に対して先に行います。
そのため最初のチャンクが届くのは変換の終盤で、ピークメモリもブロックを渡さない場合と変わりません。
HTMLを読みながらページを確定させたい場合は[ストリーミングモード](#メモリを抑えたいとき)と併せて使ってください。

1回に渡すバイト数の目安は`chunk_size:`で変えられます(既定64KiB、ローカル変換のみ)。
小さくすると細かく届きますが、そのたびにGVLを取り直すのでレンダリングは遅くなります。

```ruby
Sghtmltopdf.render(html, chunk_size: 8 * 1024) { |bytes| ... }
```

### `Thread#kill`とタイムアウトが効く

ブロックの呼び出しはRubyのメソッド呼び出しなので、その時点で保留中の割り込みが処理されます。
ブロック付きで呼んでいる限り、`Thread#kill`や`Timeout.timeout`・`Rack::Timeout`がチャンク境界で効きます。

```ruby
Timeout.timeout(10) do
  Sghtmltopdf.render(huge_html) { |bytes| io.write(bytes) }   # 10秒で中断できる
end
```

ブロックを渡さない`render`/`render_to_file`は変換の間まったくRubyへ戻らないため、途中で止められません。
長い変換に上限をかけたい場合はブロック付きで呼んでください。

### Railsで逐次返却する

`render pdf:`のレンダラは、組み上がったPDFを`send_data`で一括返却します。
確定したページから順に返したい場合は`ActionController::Live`と組み合わせます。

```ruby
class InvoicesController < ApplicationController
  include ActionController::Live

  def show
    response.headers["Content-Type"] = "application/pdf"
    html = render_to_string(template: "invoices/show", layout: "pdf")
    Sghtmltopdf.render(html) { |bytes| response.stream.write(bytes) }
  ensure
    response.stream.close
  end
end
```

途中まで書き出したあとに失敗すると、クライアントには壊れたPDFが届きます(ヘッダは既に送信済みなのでステータスを変えられません)。
サーバモードの`?stream=1`と同じ性質です。

### S3へ直接上げる

gemはS3向けの実装を持ちません(依存を増やさず、書き方も短いためです)。
マルチパートアップロードは最後のパート以外は5MB以上という制約があるので、溜めてから上げます。

```ruby
s3 = Aws::S3::Client.new
upload = s3.create_multipart_upload(bucket: bucket, key: key, content_type: "application/pdf")
parts, buffer = [], +"".b

flush = lambda do
  part = s3.upload_part(bucket: bucket, key: key, upload_id: upload.upload_id,
    part_number: parts.size + 1, body: buffer)
  parts << {part_number: parts.size + 1, etag: part.etag}
  buffer.clear
end

begin
  Sghtmltopdf.render(html, server_url: server_url) do |bytes|
    buffer << bytes
    flush.call if buffer.bytesize >= 5 * 1024 * 1024
  end
  flush.call unless buffer.empty?
  s3.complete_multipart_upload(bucket: bucket, key: key, upload_id: upload.upload_id,
    multipart_upload: {parts: parts})
rescue StandardError
  s3.abort_multipart_upload(bucket: bucket, key: key, upload_id: upload.upload_id)
  raise
end
```

小さいPDFなら`put_object(body: Sghtmltopdf.render(html))`で十分です。

## メモリを抑えたいとき

数万要素規模のHTMLでは、エンジンの[ストリーミングモード](./cli/streaming.md)を使うとメモリが大きく減ります(実測: 60,000要素で 228MB → 28MB。[メモリと処理時間](./cli/streaming.md#メモリと処理時間))。

```ruby
Sghtmltopdf.render(html, streaming: true)
```

その代わり、文書全体を見ないと決まらないもの(`toc`・`counter(pages)`・`<body>`より後の`<style>`など)が使えません。
制約の一覧は[ストリーミングモード](./cli/streaming.md)を参照してください。
