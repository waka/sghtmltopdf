# frozen_string_literal: true

require "fileutils"
require "rails_helper"
require "tmpdir"

# Confirm a PDF comes back from a controller of the dummy Rails app (spec/dummy).
RSpec.describe "a Rails controller", type: :rails do
  # Used to exercise the `@font-face` path. Not kept in the dummy app;
  # each example copies it into `public/` and removes it afterwards.
  FONT_FIXTURE = File.expand_path("../../../core/tests/fonts/DejaVuSansMono.ttf", __dir__)

  describe "render_to_string pdf:" do
    let(:controller) do
      InvoicesController.new.tap do |instance|
        instance.set_request!(ActionDispatch::TestRequest.create)
        instance.set_response!(ActionDispatch::TestResponse.new)
      end
    end

    it "returns PDF bytes without changing the response and allows send_data afterwards" do
      options = {pdf: "invoice", template: "invoices/show", layout: "pdf",
                 page_size: "A5", filename: "unused.pdf", status: 201}
      headers = controller.response.headers.to_h.dup
      pdf = controller.render_to_string(**options)
      html = controller.render_to_string(template: "invoices/show", layout: "pdf")

      expect(normalize(pdf)).to eq(normalize(Sghtmltopdf.render(html, page_size: "A5")))
      expect(controller.response_body).to be_nil
      expect(controller.performed?).to be_falsey
      expect(controller.response.status).to eq(200)
      expect(controller.response.headers.to_h).to eq(headers)
      expect(options[:pdf]).to eq("invoice")

      controller.send(:send_data, pdf, filename: "invoice.pdf", type: "application/pdf")
      expect(controller.response_body.join).to eq(pdf)
    end

    it "accepts a positional template name without changing the options or response" do
      options = {pdf: "invoice", layout: "pdf", page_size: "A5",
                 filename: "unused.pdf", status: 201}.freeze
      headers = controller.response.headers.to_h.dup
      pdf = controller.render_to_string("invoices/show", options)
      html = controller.render_to_string(template: "invoices/show", layout: "pdf")

      expect(normalize(pdf)).to eq(normalize(Sghtmltopdf.render(html, page_size: "A5")))
      expect(controller.response_body).to be_nil
      expect(controller.performed?).to be_falsey
      expect(controller.response.status).to eq(200)
      expect(controller.response.headers.to_h).to eq(headers)

      controller.send(:send_data, pdf, filename: "invoice.pdf", type: "application/pdf")
      expect(controller.response_body.join).to eq(pdf)
    end

    it "returns HTML with show_as_html without setting a response body" do
      html = controller.render_to_string(pdf: "invoice", template: "invoices/show", show_as_html: true)

      expect(html).to include("<h1>Invoice #1234</h1>")
      expect(controller.response_body).to be_nil
    end
  end

  describe "render pdf:" do
    it "returns a PDF" do
      get "/invoices/show"

      expect(last_response.status).to eq(200)
      expect(last_response.headers["content-type"]).to start_with("application/pdf")
      expect(last_response.body).to start_with("%PDF-")
      expect(last_response.body).to end_with("%%EOF")
    end

    it "defaults Content-Disposition to inline, with the value of pdf: as the file name" do
      get "/invoices/show"

      expect(last_response.headers["content-disposition"])
        .to start_with('inline; filename="invoice.pdf"')
    end

    it "converts the view's rendering result unchanged" do
      get "/invoices/show"
      html = InvoicesController.render(template: "invoices/show", layout: false)

      expect(normalize(last_response.body)).to eq(normalize(Sghtmltopdf.render(html)))
    end
  end

  # Copy the `examples/` sample (a realistic business document reading external CSS through a
  # `<link>`) into the dummy app's views and public/, and see that Rails gives the same PDF.
  #
  # There is no byte comparison against a checked-in PDF: the `font-family` in
  # `examples/main.css` looks system fonts up by name, so the output bytes change with the
  # fonts installed on the host. Instead it is matched against `Sghtmltopdf.render` in the
  # same process. Both go through the same font resolution, so it is environment-independent
  # while still catching a regression where "the Rails integration layer drops some HTML or
  # an option".
  #
  # They are copies rather than symlinks because `--allow-path` decides on the real path a symlink
  # leads to. A symlink pointing outside `Rails.root` would be rejected along with the CSS.
  describe "reproducing examples/receipt.html" do
    def example(name)
      File.binread(File.expand_path("../../../examples/#{name}", __dir__))
    end

    it "has the view and public/main.css identical to examples/" do
      expect(File.binread(Rails.root.join("app/views/invoices/receipt.html.erb")))
        .to eq(example("receipt.html"))
      expect(File.binread(Rails.root.join("public/main.css"))).to eq(example("main.css"))
    end

    it "gives the same PDF as converting directly" do
      get "/invoices/receipt"
      html = InvoicesController.render(template: "invoices/receipt", layout: false)

      expect(last_response.status).to eq(200)
      expect(normalize(last_response.body)).to eq(normalize(Sghtmltopdf.render(html)))
    end

    it "really applies public/main.css" do
      get "/invoices/receipt"
      styled = last_response.body
      # With base_url pointing at an empty directory, main.css cannot be resolved (a failed
      # fetch is ignored by default). The same HTML giving a different result guarantees the
      # spec above is not passing with neither CSS applied.
      html = InvoicesController.render(template: "invoices/receipt", layout: false)
      unstyled = Dir.mktmpdir { |dir| Sghtmltopdf.render(html, base_url: dir) }

      expect(normalize(styled)).not_to eq(normalize(unstyled))
    end
  end

  describe "passing the options through" do
    it "puts filename/disposition in the response" do
      get "/invoices/download"

      disposition = last_response.headers["content-disposition"]
      expect(disposition).to start_with("attachment;")
      # A Japanese file name also appears as RFC 5987's filename*.
      expect(disposition).to include("filename*=UTF-8''")
    end

    it "passes the conversion options to the PDF" do
      get "/invoices/download"
      a5 = last_response.body
      get "/invoices/show"
      a4 = last_response.body

      # The download side uses page_size: "A5". A different paper size gives different content.
      expect(normalize(a5)).not_to eq(normalize(a4))
    end

    it "honours layout:" do
      get "/invoices/with_layout"
      with_layout = last_response.body
      get "/invoices/show"
      without_layout = last_response.body

      expect(normalize(with_layout)).not_to eq(normalize(without_layout))
    end

    it "returns HTML with show_as_html" do
      get "/invoices/as_html"

      expect(last_response.headers["content-type"]).to start_with("text/html")
      expect(last_response.body).to include("<h1>Invoice #1234</h1>")
    end

    it "makes an unknown option a Sghtmltopdf::UsageError" do
      expect { get "/invoices/bad_option" }
        .to raise_error(Sghtmltopdf::UsageError, /--no-such-option/)
    end
  end

  describe "the Rails-oriented default options" do
    it "has the Railtie inject base_url and allow_path" do
      expect(CONFIG_AFTER_BOOT[:base_url]).to eq(Rails.root.join("public").to_s)
      # allow_path is public/ plus the pipeline load paths. The dummy app has no
      # pipeline gem, so it is just public/ (pipeline_spec.rb covers the gem case).
      expect(CONFIG_AFTER_BOOT[:allow_path]).to eq([Rails.root.join("public").to_s])
    end

    it "lets a later setting such as config/initializers override them" do
      Sghtmltopdf.configure { |c| c.base_url = "/somewhere/else" }

      expect(Sghtmltopdf.config[:base_url]).to eq("/somewhere/else")
    end

    it "resolves the CSS in public/ through the base_url default" do
      html = '<link rel="stylesheet" href="/invoice.css"><h1>Invoice</h1>'
      # With the default (Rails.root/public), invoice.css is readable. With an empty
      # directory as base_url it is not (a failed fetch is ignored by default).
      resolved = Sghtmltopdf.render(html)
      missing = Dir.mktmpdir { |dir| Sghtmltopdf.render(html, base_url: dir) }

      expect(normalize(resolved)).not_to eq(normalize(missing))
    end

    it "does not read a file outside the allowed directories under the allow_path default" do
      Dir.mktmpdir do |dir|
        File.write(File.join(dir, "outside.css"), "h1 { font-size: 48px }")
        html = '<link rel="stylesheet" href="outside.css"><h1>Invoice</h1>'

        blocked = Sghtmltopdf.render(html, base_url: dir)
        allowed = Sghtmltopdf.render(html, base_url: dir, allow: [dir])

        expect(normalize(blocked)).not_to eq(normalize(allowed))
      end
    end
  end

  # Combine a block-taking render with ActionController::Live and stream to the Rack response
  # page by page as they settle.
  #
  # Rack::Test's `last_response.body` cannot be used: `MockResponse` stops at the first chunk
  # rather than reading a streaming body to the end, so the Rack body is `each`ed by hand.
  describe "streaming to Rack" do
    def stream_response(path)
      status, headers, body = app.call(Rack::MockRequest.env_for(path))
      chunks = []
      body.each { |part| chunks << part }
      body.close if body.respond_to?(:close)
      [status, headers, chunks]
    end

    it "writes to response.stream chunk by chunk" do
      status, headers, chunks = stream_response("/streams/show")

      expect(status).to eq(200)
      expect(headers["content-type"]).to start_with("application/pdf")
      # It is not one single write.
      expect(chunks.size).to be > 1
      expect(chunks.first).to start_with("%PDF-")
      expect(chunks.last).to end_with("%%EOF")
    end

    it "gives the same PDF as a one-shot conversion" do
      _status, _headers, chunks = stream_response("/streams/show")
      html = StreamsController.render(template: "invoices/long", layout: false)

      expect(normalize(chunks.join)).to eq(normalize(Sghtmltopdf.render(html)))
    end
  end

  describe "delegating to server mode" do
    it "can delegate to a server from a controller too, without sending the Rails defaults" do
      FakeServer.run do |server|
        Sghtmltopdf.configure { |c| c.server_url = server.url }
        get "/invoices/show"

        expect(last_response.status).to eq(200)
        expect(last_response.body).to start_with("%PDF-")
        # The `base_url`/`allow_path` the Railtie injects are keys the server cannot be given, so
        # sending them would give a 400.
        expect(server.last_request.query).to eq("")
        expect(server.last_request.body).to include("<h1>Invoice #1234</h1>")
      end
    end
  end

  describe "the view helpers" do
    it "inlines CSS from public/ into a <style>" do
      get "/invoices/with_stylesheet"
      inlined = last_response.body

      # A PDF through the helper differs from the same HTML with no CSS applied.
      plain = Sghtmltopdf.render("<h1>Invoice</h1>")

      expect(normalize(inlined)).not_to eq(normalize(plain))
    end

    it "returns nil for an asset that is not found" do
      view = InvoicesController.new.view_context

      expect(view.sghtmltopdf_asset_path("no-such-file.css")).to be_nil
      expect(view.sghtmltopdf_asset_path("invoice.css")).to eq(Rails.root.join("public/invoice.css").to_s)
    end

    describe "sghtmltopdf_image_tag" do
      let(:view) { InvoicesController.new.view_context }

      it "points at an image in public/ by a path relative to base_url" do
        html = view.sghtmltopdf_image_tag("logo.png")

        expect(html).to eq(%(<img src="logo.png">))
      end

      it "embeds a data URI with inline: true" do
        html = view.sghtmltopdf_image_tag("logo.png", inline: true)

        expect(html).to include(%(src="data:image/png;base64,))
        expect(html).to include([File.binread(Rails.root.join("public/logo.png"))].pack("m0"))
      end

      # #44: `image_tag` used to be given a file path, so a host in
      # `default_url_options` turned it into a URL that the engine tried, and
      # failed, to fetch remotely.
      it "does not become a URL even with a host in default_url_options" do
        Rails.application.routes.default_url_options[:host] = "localhost:3000"

        expect(view.sghtmltopdf_image_tag("logo.png")).to eq(%(<img src="logo.png">))
        expect(view.sghtmltopdf_image_tag("logo.png", inline: true)).not_to include("http://")
      ensure
        Rails.application.routes.default_url_options.delete(:host)
      end

      it "expands size: into width/height" do
        html = view.sghtmltopdf_image_tag("logo.png", size: "40x30")

        expect(html).to include(%(width="40"))
        expect(html).to include(%(height="30"))
        expect(html).not_to include("size=")
      end

      it "passes options through as attributes" do
        html = view.sghtmltopdf_image_tag("logo.png", class: "seal", alt: "ロゴ")

        expect(html).to include(%(class="seal"))
        expect(html).to include(%(alt="ロゴ"))
      end

      # Paths are now the default, so `inline: false` means the same as the default.
      # It is still accepted for callers that explicitly opted out of the old default (embedding).
      it "emits a path with inline: false, like the default" do
        html = view.sghtmltopdf_image_tag("logo.png", inline: false, class: "seal")

        expect(html).to include(%(src="logo.png"))
        expect(html).to include(%(class="seal"))
      end

      # The engine cannot read a file outside allow_path even when pointed at it.
      # A failed fetch is ignored by default, so fall back to embedding rather than vanish silently.
      it "falls back to embedding for a file outside allow_path" do
        outside = Rails.root.join("app/assets/images/pipeline-logo.png").to_s

        html = view.sghtmltopdf_image_tag(outside)

        expect(html).to include("data:image/png;base64,")
      end

      # When delegating to a server, a local path may not exist on its
      # filesystem. An embedded image reads wherever it is rendered.
      it "falls back to embedding when server_url is set" do
        Sghtmltopdf.configure { |c| c.server_url = "http://127.0.0.1:1" }

        expect(view.sghtmltopdf_image_tag("logo.png")).to include("data:image/png;base64,")
      end

      it "leaves anything that is not an app asset to Rails" do
        html = view.sghtmltopdf_image_tag("https://example.com/logo.png")

        expect(html).to include(%(src="https://example.com/logo.png"))
      end

      it "emits a default path the engine can resolve" do
        html = view.sghtmltopdf_image_tag("logo.png")

        # Readable as a path relative to the default `base_url` (Rails.root/public).
        expect(Sghtmltopdf.render(html)).to include("/Subtype /Image")
      end

      it "puts the image from the helper into the PDF" do
        get "/invoices/with_image"

        expect(last_response.body).to start_with("%PDF-")
        # The 20x16 PNG is embedded as an XObject.
        expect(last_response.body).to include("/Subtype /Image")
        expect(last_response.body).to include("/Width 20")
        expect(last_response.body).to include("/Height 16")
      end
    end

    # #45: the `url()`s in precompiled CSS have already been rewritten by the
    # pipeline through `asset_path`, so with an `asset_host` they become absolute
    # HTTPS URLs. PDF rendering never goes through the HTTP server, so they
    # cannot be fetched and `@font-face` silently falls back to the default
    # font. The helper points them back at files on disk.
    describe "sghtmltopdf_stylesheet_link_tag" do
      let(:view) { InvoicesController.new.view_context }

      # To avoid leaving the font in the dummy app, build the set under
      # `public/` and remove it after each example.
      around do |example|
        @dir = Rails.root.join("public/css-fixtures")
        FileUtils.mkdir_p(@dir.join("fonts"))
        FileUtils.cp(FONT_FIXTURE, @dir.join("fonts/gyre.ttf"))
        FileUtils.cp(Rails.root.join("public/logo.png"), @dir.join("seal.png"))
        example.run
      ensure
        FileUtils.rm_rf(@dir)
      end

      # Writes `css` to `public/css-fixtures/main.css` and returns the helper's output.
      def inline(css, name: "main")
        File.write(@dir.join("#{name}.css"), css)
        view.sghtmltopdf_stylesheet_link_tag("css-fixtures/#{name}")
      end

      it "points an absolute URL with asset_host back at the local file" do
        html = inline(<<~CSS)
          @font-face {
            font-family: "Gyre";
            src: url(https://cdn.example.com/css-fixtures/fonts/gyre.ttf);
          }
        CSS

        expect(html).to include(%(url("css-fixtures/fonts/gyre.ttf")))
        expect(html).not_to include("https://")
      end

      it "points a root-relative reference back at the local file" do
        html = inline(%(body { background-image: url("/css-fixtures/seal.png"); }))

        expect(html).to include(%(url("css-fixtures/seal.png")))
      end

      # The engine concatenates every CSS source before resolving, so a relative
      # `url()` resolves against the document's base_url. Only this side knows
      # the CSS file's real path, so it is resolved here before being spliced in.
      it "resolves a relative reference against the CSS file's own directory" do
        html = inline(%(body { background-image: url(seal.png); }))

        expect(html).to include(%(url("css-fixtures/seal.png")))
      end

      it "resolves a reference that climbs up with .." do
        html = inline(%(body { background-image: url("../../logo.png"); }), name: "fonts/deep")

        expect(html).to include(%(url("logo.png")))
      end

      it "drops the query and the fragment" do
        html = inline(%(@font-face { src: url(fonts/gyre.ttf?v=2#iefix); }))

        expect(html).to include(%(url("css-fixtures/fonts/gyre.ttf")))
        expect(html).not_to include("iefix")
      end

      it "passes data: URIs and bare fragments through" do
        html = inline(<<~CSS)
          @font-face { src: url(data:font/ttf;base64,AAEAAA); }
          .mask { mask: url(#clip); }
        CSS

        expect(html).to include("url(data:font/ttf;base64,AAEAAA)")
        expect(html).to include("url(#clip)")
      end

      it "passes a remote URL with no local file through" do
        html = inline(%(@font-face { src: url(https://fonts.gstatic.com/s/x.woff2); }))

        expect(html).to include("url(https://fonts.gstatic.com/s/x.woff2)")
      end

      # `local()` is not a file reference, so it is left alone.
      it "does not touch local()" do
        html = inline(%(@font-face { src: local("Gyre"), url(fonts/gyre.ttf); }))

        expect(html).to include(%(local("Gyre")))
        expect(html).to include(%(url("css-fixtures/fonts/gyre.ttf")))
      end

      # Pointing at an unreadable file by path makes it vanish silently, since a
      # failed fetch is ignored by default. `@font-face` does not abort even with
      # `abort`, all the more reason to fall back to embedding.
      it "falls back to embedding for a file the engine cannot read" do
        Sghtmltopdf.configure { |c| c.server_url = "http://127.0.0.1:1" }

        html = inline(%(@font-face { src: url(fonts/gyre.ttf); }))

        expect(html).to include("data:font/ttf;base64,")
      end

      it "expands @import recursively and rewrites the url()s in imported files too" do
        File.write(@dir.join("fonts/child.css"), %(body { background-image: url(../seal.png); }))
        html = inline(%(@import url("fonts/child.css");\nh1 { color: red; }))

        expect(html).not_to include("@import")
        expect(html).to include(%(url("css-fixtures/seal.png")))
        expect(html).to include("h1 { color: red; }")
      end

      it "expands a quoted-string @import and one with a media condition too" do
        File.write(@dir.join("a.css"), "h1 { color: red; }")
        File.write(@dir.join("b.css"), "h2 { color: blue; }")
        html = inline(%(@import "a.css";\n@import url(b.css) print;))

        expect(html).to include("h1 { color: red; }")
        expect(html).to include("h2 { color: blue; }")
        expect(html).not_to include("print")
      end

      it "does not expand a commented-out @import" do
        File.write(@dir.join("a.css"), "h1 { color: red; }")
        html = inline(%(/* @import "a.css"; */\nh2 { color: blue; }))

        expect(html).not_to include("color: red")
        expect(html).to include(%(/* @import "a.css"; */))
      end

      # A CSS file that imports its own ancestor would multiply the reads by each
      # branch if expanded to the depth cap. A file already in the chain stops
      # there and is left to the engine.
      it "leaves a circular @import as it is" do
        File.write(@dir.join("a.css"), %(@import "main.css";\nh1 { color: red; }))
        html = inline(%(@import url("a.css");))

        expect(html).to include("h1 { color: red; }")
        expect(html).to include(%(@import "main.css";))
      end

      it "leaves an @import with no local file for the engine" do
        html = inline(%(@import url("https://example.com/x.css");))

        expect(html).to include(%(@import url("https://example.com/x.css");))
      end

      # This checks the #45 symptom itself: a failed fetch falls back to the engine default
      # rather than the next `font-family` candidate. The fixture is embedded under its own
      # PostScript name (DejaVuSansMono), not under the "Gyre" family the CSS gives it.
      it "actually applies the font of the redirected @font-face" do
        css = <<~CSS
          @font-face {
            font-family: "Gyre";
            src: url(https://cdn.example.com/css-fixtures/fonts/gyre.ttf);
          }
          body { font-family: "Gyre"; }
        CSS
        body = "<p>Hello</p>"

        rewritten = Sghtmltopdf.render(inline(css) + body)
        # The CSS spliced in before rewriting (the behavior before the fix).
        verbatim = Sghtmltopdf.render(%(<style type="text/css">#{css}</style>#{body}))
        without = Sghtmltopdf.render(body)

        expect(rewritten).to include("/FontFile2")
        expect(rewritten).to include("+DejaVuSansMono")
        expect(verbatim).not_to include("+DejaVuSansMono")
        expect(normalize(verbatim)).to eq(normalize(without))
        expect(normalize(rewritten)).not_to eq(normalize(without))
      end
    end
  end
end
