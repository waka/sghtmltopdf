# frozen_string_literal: true

require "rails_helper"
require "tmpdir"

# Confirm a PDF comes back from a controller of the dummy Rails app (spec/dummy).
RSpec.describe "a Rails controller", type: :rails do
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
  # They are copies rather than symlinks because `--allow` decides on the real path a symlink
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
    it "has the Railtie inject base_url and allow" do
      expect(CONFIG_AFTER_BOOT[:base_url]).to eq(Rails.root.join("public").to_s)
      expect(CONFIG_AFTER_BOOT[:allow]).to eq([Rails.root.to_s])
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

    it "does not read a file outside Rails.root under the allow default" do
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
        # The `base_url`/`allow` the Railtie injects are keys the server cannot be given, so
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
  end
end
