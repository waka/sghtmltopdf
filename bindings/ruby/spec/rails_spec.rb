# frozen_string_literal: true

require "fileutils"
require "rails_helper"
require "tmpdir"

# ダミーのRailsアプリ(spec/dummy)のコントローラからPDFが返ること。
RSpec.describe "Railsのコントローラ", type: :rails do
  # `@font-face`の経路を見るために使う。ダミーアプリには置かず、
  # 例ごとに`public/`へ複製して消す。
  FONT_FIXTURE = File.expand_path("../../../core/tests/fonts/DejaVuSansMono.ttf", __dir__)

  describe "render pdf:" do
    it "PDFを返す" do
      get "/invoices/show"

      expect(last_response.status).to eq(200)
      expect(last_response.headers["content-type"]).to start_with("application/pdf")
      expect(last_response.body).to start_with("%PDF-")
      expect(last_response.body).to end_with("%%EOF")
    end

    it "既定のContent-Dispositionはinlineで、pdf:の値がファイル名になる" do
      get "/invoices/show"

      expect(last_response.headers["content-disposition"])
        .to start_with('inline; filename="invoice.pdf"')
    end

    it "ビューの描画結果がそのまま変換される" do
      get "/invoices/show"
      html = InvoicesController.render(template: "invoices/show", layout: false)

      expect(normalize(last_response.body)).to eq(normalize(Sghtmltopdf.render(html)))
    end
  end

  # `examples/`のサンプル(外部CSSを`<link>`で読む、実務に近い帳票)を
  # dummyアプリのビューとpublic/へ複製し、Rails経由でも同じPDFになることを見る。
  #
  # チェックインしたPDFとのバイト比較はしない。`examples/main.css`の
  # `font-family`はシステムフォントを名前で引くため、出力バイトがホストに
  # 入っているフォントで変わってしまう。代わりに同一プロセス内の
  # `Sghtmltopdf.render`と突き合わせる。どちらも同じフォント解決を通るので
  # 環境に依存せず、それでいて「Rails統合層がHTMLかオプションを取りこぼす」
  # 退行はきちんと捕まえられる。
  #
  # symlinkではなく複製にしているのは、`--allow-path`がsymlinkを辿った先の実体
  # パスで判定するため。`Rails.root`の外を指すsymlinkはCSSごと弾かれる。
  describe "examples/receipt.htmlの再現" do
    def example(name)
      File.binread(File.expand_path("../../../examples/#{name}", __dir__))
    end

    it "ビューとpublic/main.cssがexamples/と同じ内容である" do
      expect(File.binread(Rails.root.join("app/views/invoices/receipt.html.erb")))
        .to eq(example("receipt.html"))
      expect(File.binread(Rails.root.join("public/main.css"))).to eq(example("main.css"))
    end

    it "直接変換した場合と同じPDFになる" do
      get "/invoices/receipt"
      html = InvoicesController.render(template: "invoices/receipt", layout: false)

      expect(last_response.status).to eq(200)
      expect(normalize(last_response.body)).to eq(normalize(Sghtmltopdf.render(html)))
    end

    it "public/main.cssが実際に当たっている" do
      get "/invoices/receipt"
      styled = last_response.body
      # base_urlを空のディレクトリにするとmain.cssを解決できない(取得失敗は
      # 既定で無視される)。同じHTMLでも結果が変わることで、上のspecが
      # 「CSSが両方とも当たっていない」状態で通っていないことを担保する。
      html = InvoicesController.render(template: "invoices/receipt", layout: false)
      unstyled = Dir.mktmpdir { |dir| Sghtmltopdf.render(html, base_url: dir) }

      expect(normalize(styled)).not_to eq(normalize(unstyled))
    end
  end

  describe "オプションの受け渡し" do
    it "filename/dispositionがレスポンスに出る" do
      get "/invoices/download"

      disposition = last_response.headers["content-disposition"]
      expect(disposition).to start_with("attachment;")
      # 日本語のファイル名はRFC 5987のfilename*としても出る。
      expect(disposition).to include("filename*=UTF-8''")
    end

    it "変換オプションはPDFへ渡る" do
      get "/invoices/download"
      a5 = last_response.body
      get "/invoices/show"
      a4 = last_response.body

      # download側は page_size: "A5"。用紙サイズが違えば中身も違う。
      expect(normalize(a5)).not_to eq(normalize(a4))
    end

    it "layout:が効く" do
      get "/invoices/with_layout"
      with_layout = last_response.body
      get "/invoices/show"
      without_layout = last_response.body

      expect(normalize(with_layout)).not_to eq(normalize(without_layout))
    end

    it "show_as_htmlならHTMLを返す" do
      get "/invoices/as_html"

      expect(last_response.headers["content-type"]).to start_with("text/html")
      expect(last_response.body).to include("<h1>Invoice #1234</h1>")
    end

    it "未知のオプションはSghtmltopdf::UsageErrorになる" do
      expect { get "/invoices/bad_option" }
        .to raise_error(Sghtmltopdf::UsageError, /--no-such-option/)
    end
  end

  describe "Rails向けの既定オプション" do
    it "Railtieがbase_urlとallow_pathを入れる" do
      expect(CONFIG_AFTER_BOOT[:base_url]).to eq(Rails.root.join("public").to_s)
      # allow_pathはpublic/とパイプラインのロードパス。dummyアプリはパイプライン
      # gemを入れていないのでpublic/だけになる(gemがある場合はpipeline_spec.rb)。
      expect(CONFIG_AFTER_BOOT[:allow_path]).to eq([Rails.root.join("public").to_s])
    end

    it "config/initializersなど後からの設定で上書きできる" do
      Sghtmltopdf.configure { |c| c.base_url = "/somewhere/else" }

      expect(Sghtmltopdf.config[:base_url]).to eq("/somewhere/else")
    end

    it "base_urlの既定でpublic/のCSSが解決される" do
      html = '<link rel="stylesheet" href="/invoice.css"><h1>Invoice</h1>'
      # 既定(Rails.root/public)ならinvoice.cssが読める。空のディレクトリを
      # base_urlにすると読めない(取得失敗は既定で無視される)。
      resolved = Sghtmltopdf.render(html)
      missing = Dir.mktmpdir { |dir| Sghtmltopdf.render(html, base_url: dir) }

      expect(normalize(resolved)).not_to eq(normalize(missing))
    end

    it "allow_pathの既定では許可ディレクトリの外のファイルを読まない" do
      Dir.mktmpdir do |dir|
        File.write(File.join(dir, "outside.css"), "h1 { font-size: 48px }")
        html = '<link rel="stylesheet" href="outside.css"><h1>Invoice</h1>'

        blocked = Sghtmltopdf.render(html, base_url: dir)
        allowed = Sghtmltopdf.render(html, base_url: dir, allow: [dir])

        expect(normalize(blocked)).not_to eq(normalize(allowed))
      end
    end
  end

  # ブロック付きrenderをActionController::Liveと組み合わせて、
  # 確定したページから順にRackのレスポンスへ流せること。
  #
  # Rack::Testの`last_response.body`は使えない。`MockResponse`は
  # ストリーミングのボディを読み切らずに最初のチャンクで止まるため、
  # Rackのボディを自分で`each`する。
  describe "Rackへのストリーミング" do
    def stream_response(path)
      status, headers, body = app.call(Rack::MockRequest.env_for(path))
      chunks = []
      body.each { |part| chunks << part }
      body.close if body.respond_to?(:close)
      [status, headers, chunks]
    end

    it "response.streamへチャンクごとに書き出される" do
      status, headers, chunks = stream_response("/streams/show")

      expect(status).to eq(200)
      expect(headers["content-type"]).to start_with("application/pdf")
      # 一括で1回書き出しているのではないこと。
      expect(chunks.size).to be > 1
      expect(chunks.first).to start_with("%PDF-")
      expect(chunks.last).to end_with("%%EOF")
    end

    it "一括変換と同じPDFになる" do
      _status, _headers, chunks = stream_response("/streams/show")
      html = StreamsController.render(template: "invoices/long", layout: false)

      expect(normalize(chunks.join)).to eq(normalize(Sghtmltopdf.render(html)))
    end
  end

  describe "サーバモードへの委譲" do
    it "コントローラからでもサーバへ委譲でき、Railsの既定値は送らない" do
      FakeServer.run do |server|
        Sghtmltopdf.configure { |c| c.server_url = server.url }
        get "/invoices/show"

        expect(last_response.status).to eq(200)
        expect(last_response.body).to start_with("%PDF-")
        # Railtieが入れる`base_url`/`allow_path`はサーバでは指定できないキーなので、
        # 送ってしまうと400になる。
        expect(server.last_request.query).to eq("")
        expect(server.last_request.body).to include("<h1>Invoice #1234</h1>")
      end
    end
  end

  describe "ビューヘルパ" do
    it "public/のCSSを<style>へ展開する" do
      get "/invoices/with_stylesheet"
      inlined = last_response.body

      # ヘルパを通したPDFは、CSSが当たっていない同じHTMLとは異なる。
      plain = Sghtmltopdf.render("<h1>Invoice</h1>")

      expect(normalize(inlined)).not_to eq(normalize(plain))
    end

    it "見つからないアセットはnilを返す" do
      view = InvoicesController.new.view_context

      expect(view.sghtmltopdf_asset_path("no-such-file.css")).to be_nil
      expect(view.sghtmltopdf_asset_path("invoice.css")).to eq(Rails.root.join("public/invoice.css").to_s)
    end

    describe "sghtmltopdf_image_tag" do
      let(:view) { InvoicesController.new.view_context }

      it "public/の画像はbase_url基準の相対パスで指す" do
        html = view.sghtmltopdf_image_tag("logo.png")

        expect(html).to eq(%(<img src="logo.png">))
      end

      it "inline: trueならdata URIとして埋め込む" do
        html = view.sghtmltopdf_image_tag("logo.png", inline: true)

        expect(html).to include(%(src="data:image/png;base64,))
        expect(html).to include([File.binread(Rails.root.join("public/logo.png"))].pack("m0"))
      end

      # #44: `image_tag`にファイルパスを渡していたため、`default_url_options`に
      # ホストがあるとURLに化け、エンジンがリモート取得を試みて失敗していた。
      it "default_url_optionsにホストがあってもURLにならない" do
        Rails.application.routes.default_url_options[:host] = "localhost:3000"

        expect(view.sghtmltopdf_image_tag("logo.png")).to eq(%(<img src="logo.png">))
        expect(view.sghtmltopdf_image_tag("logo.png", inline: true)).not_to include("http://")
      ensure
        Rails.application.routes.default_url_options.delete(:host)
      end

      it "size:はwidth/heightへ展開される" do
        html = view.sghtmltopdf_image_tag("logo.png", size: "40x30")

        expect(html).to include(%(width="40"))
        expect(html).to include(%(height="30"))
        expect(html).not_to include("size=")
      end

      it "オプションはそのまま属性になる" do
        html = view.sghtmltopdf_image_tag("logo.png", class: "seal", alt: "ロゴ")

        expect(html).to include(%(class="seal"))
        expect(html).to include(%(alt="ロゴ"))
      end

      # 既定がパス形式になったので`inline: false`は既定と同じ意味になる。
      # 旧既定(埋め込み)を明示的に外していた呼び出しのために受け続ける。
      it "inline: falseは既定と同じくパスを出す" do
        html = view.sghtmltopdf_image_tag("logo.png", inline: false, class: "seal")

        expect(html).to include(%(src="logo.png"))
        expect(html).to include(%(class="seal"))
      end

      # allow_pathの外にあるファイルはパスで指してもエンジンが読めない。
      # 取得失敗は既定で無視されるので、無言で消えないよう埋め込みへ倒す。
      it "allow_pathの外のファイルは埋め込みに倒す" do
        outside = Rails.root.join("app/assets/images/pipeline-logo.png").to_s

        html = view.sghtmltopdf_image_tag(outside)

        expect(html).to include("data:image/png;base64,")
      end

      # サーバへ委譲する場合、ローカルのパスは相手のファイルシステムに
      # 無いかもしれない。埋め込みならどこで描いても読める。
      it "server_urlが設定されていれば埋め込みに倒す" do
        Sghtmltopdf.configure { |c| c.server_url = "http://127.0.0.1:1" }

        expect(view.sghtmltopdf_image_tag("logo.png")).to include("data:image/png;base64,")
      end

      it "アプリのアセットでないものはRailsに任せる" do
        html = view.sghtmltopdf_image_tag("https://example.com/logo.png")

        expect(html).to include(%(src="https://example.com/logo.png"))
      end

      it "既定で出したパスをエンジンが解決できる" do
        html = view.sghtmltopdf_image_tag("logo.png")

        # `base_url`の既定(Rails.root/public)からの相対パスとして読める。
        expect(Sghtmltopdf.render(html)).to include("/Subtype /Image")
      end

      it "ヘルパで埋めた画像がPDFに入る" do
        get "/invoices/with_image"

        expect(last_response.body).to start_with("%PDF-")
        # 20x16のPNGがXObjectとして埋まっている。
        expect(last_response.body).to include("/Subtype /Image")
        expect(last_response.body).to include("/Width 20")
        expect(last_response.body).to include("/Height 16")
      end
    end

    # #45: precompileしたCSSの`url()`はパイプラインが`asset_path`で書き換えた
    # あとなので、`asset_host`があればHTTPSの絶対URLになる。PDF生成はHTTP
    # サーバを通らないので取得できず、`@font-face`は無言で既定フォントに
    # 落ちる。ヘルパはこれをディスク上のファイルへ指し直す。
    describe "sghtmltopdf_stylesheet_link_tag" do
      let(:view) { InvoicesController.new.view_context }

      # フォントをダミーアプリに置きっぱなしにしないよう、`public/`の下へ
      # 一式を作って例ごとに消す。
      around do |example|
        @dir = Rails.root.join("public/css-fixtures")
        FileUtils.mkdir_p(@dir.join("fonts"))
        FileUtils.cp(FONT_FIXTURE, @dir.join("fonts/gyre.ttf"))
        FileUtils.cp(Rails.root.join("public/logo.png"), @dir.join("seal.png"))
        example.run
      ensure
        FileUtils.rm_rf(@dir)
      end

      # `public/css-fixtures/main.css`に`css`を書いて、ヘルパの出力を返す。
      def inline(css, name: "main")
        File.write(@dir.join("#{name}.css"), css)
        view.sghtmltopdf_stylesheet_link_tag("css-fixtures/#{name}")
      end

      it "asset_hostのついた絶対URLをローカルのファイルへ指し直す" do
        html = inline(<<~CSS)
          @font-face {
            font-family: "Gyre";
            src: url(https://cdn.example.com/css-fixtures/fonts/gyre.ttf);
          }
        CSS

        expect(html).to include(%(url("css-fixtures/fonts/gyre.ttf")))
        expect(html).not_to include("https://")
      end

      it "ルート相対の参照をローカルのファイルへ指し直す" do
        html = inline(%(body { background-image: url("/css-fixtures/seal.png"); }))

        expect(html).to include(%(url("css-fixtures/seal.png")))
      end

      # エンジンは全CSSソースを連結してから解決するので、相対`url()`は
      # 文書のbase_url基準になる。CSSファイルの実パスを知っているのは
      # こちら側だけなので、ここで解決してから流し込む。
      it "相対参照はCSSファイル自身のディレクトリ基準で解決する" do
        html = inline(%(body { background-image: url(seal.png); }))

        expect(html).to include(%(url("css-fixtures/seal.png")))
      end

      it "..で親をたどる参照も解決する" do
        html = inline(%(body { background-image: url("../../logo.png"); }), name: "fonts/deep")

        expect(html).to include(%(url("logo.png")))
      end

      it "クエリとフラグメントは落とす" do
        html = inline(%(@font-face { src: url(fonts/gyre.ttf?v=2#iefix); }))

        expect(html).to include(%(url("css-fixtures/fonts/gyre.ttf")))
        expect(html).not_to include("iefix")
      end

      it "data: URIと素のフラグメントは素通しする" do
        html = inline(<<~CSS)
          @font-face { src: url(data:font/ttf;base64,AAEAAA); }
          .mask { mask: url(#clip); }
        CSS

        expect(html).to include("url(data:font/ttf;base64,AAEAAA)")
        expect(html).to include("url(#clip)")
      end

      it "ローカルに無いリモートURLは素通しする" do
        html = inline(%(@font-face { src: url(https://fonts.gstatic.com/s/x.woff2); }))

        expect(html).to include("url(https://fonts.gstatic.com/s/x.woff2)")
      end

      # `local()`はファイル参照ではないので触らない。
      it "local()には手を付けない" do
        html = inline(%(@font-face { src: local("Gyre"), url(fonts/gyre.ttf); }))

        expect(html).to include(%(local("Gyre")))
        expect(html).to include(%(url("css-fixtures/fonts/gyre.ttf")))
      end

      # 読めない場所のファイルをパスで指すと、取得失敗が既定で無視される
      # ぶん無言で消える。`@font-face`は`abort`にしても中断されないので、
      # なおさら埋め込みへ倒す。
      it "エンジンが読めない場所のファイルは埋め込みに倒す" do
        Sghtmltopdf.configure { |c| c.server_url = "http://127.0.0.1:1" }

        html = inline(%(@font-face { src: url(fonts/gyre.ttf); }))

        expect(html).to include("data:font/ttf;base64,")
      end

      it "@importを再帰的に展開し、取り込んだ先のurl()も書き換える" do
        File.write(@dir.join("fonts/child.css"), %(body { background-image: url(../seal.png); }))
        html = inline(%(@import url("fonts/child.css");\nh1 { color: red; }))

        expect(html).not_to include("@import")
        expect(html).to include(%(url("css-fixtures/seal.png")))
        expect(html).to include("h1 { color: red; }")
      end

      it "引用符だけの@importとメディア条件つきの@importも展開する" do
        File.write(@dir.join("a.css"), "h1 { color: red; }")
        File.write(@dir.join("b.css"), "h2 { color: blue; }")
        html = inline(%(@import "a.css";\n@import url(b.css) print;))

        expect(html).to include("h1 { color: red; }")
        expect(html).to include("h2 { color: blue; }")
        expect(html).not_to include("print")
      end

      it "コメントアウトされた@importは展開しない" do
        File.write(@dir.join("a.css"), "h1 { color: red; }")
        html = inline(%(/* @import "a.css"; */\nh2 { color: blue; }))

        expect(html).not_to include("color: red")
        expect(html).to include(%(/* @import "a.css"; */))
      end

      # 自分の祖先を取り込むCSSは、深さ上限まで展開すると読み込み回数が
      # 分岐ぶん膨らむ。連鎖に出てきたファイルはそこで止めてエンジンに任せる。
      it "循環した@importはそのまま残す" do
        File.write(@dir.join("a.css"), %(@import "main.css";\nh1 { color: red; }))
        html = inline(%(@import url("a.css");))

        expect(html).to include("h1 { color: red; }")
        expect(html).to include(%(@import "main.css";))
      end

      it "ローカルに無い@importはそのまま残してエンジンに任せる" do
        html = inline(%(@import url("https://example.com/x.css");))

        expect(html).to include(%(@import url("https://example.com/x.css");))
      end

      # 埋め込まれたフォントはPDF上どれも`/EmbeddedFont`という名前になるので、
      # 名前では見分けられない。#45の症状そのもの、つまり「取得に失敗すると
      # `font-family`の次の候補ではなくエンジン既定へ落ちる」を突き合わせる。
      it "指し直した@font-faceのフォントが実際に効く" do
        css = <<~CSS
          @font-face {
            font-family: "Gyre";
            src: url(https://cdn.example.com/css-fixtures/fonts/gyre.ttf);
          }
          body { font-family: "Gyre"; }
        CSS
        body = "<p>Hello</p>"

        rewritten = Sghtmltopdf.render(inline(css) + body)
        # 書き換える前のCSSをそのまま流し込んだ場合(修正前の挙動)。
        verbatim = Sghtmltopdf.render(%(<style type="text/css">#{css}</style>#{body}))
        without = Sghtmltopdf.render(body)

        expect(rewritten).to include("/FontFile2")
        expect(normalize(verbatim)).to eq(normalize(without))
        expect(normalize(rewritten)).not_to eq(normalize(without))
      end
    end
  end
end
