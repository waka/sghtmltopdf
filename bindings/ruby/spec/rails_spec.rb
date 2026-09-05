# frozen_string_literal: true

require "rails_helper"
require "tmpdir"

# ダミーのRailsアプリ(spec/dummy)のコントローラからPDFが返ること。
RSpec.describe "Railsのコントローラ", type: :rails do
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
  end
end
