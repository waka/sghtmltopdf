# frozen_string_literal: true

require "tmpdir"

RSpec.describe "Header/footer HTML content" do
  let(:html) { '<p>First</p><p style="break-before:page">Second</p>' }
  let(:markup) { '<div style="margin:0;color:red">[title] [custom] [page]/[topage]</div>' }
  let(:options) { {title: "Invoice", replace: "custom=Example"} }

  after { Sghtmltopdf.reset_config! }

  %i[header footer].each do |side|
    it "renders #{side} content like file input through every Ruby output API" do
      Dir.mktmpdir("sghtmltopdf-content") do |dir|
        source = File.join(dir, "overlay.html")
        output = File.join(dir, "output.pdf")
        File.write(source, markup)
        expected = normalize(Sghtmltopdf.render(html, **options, "#{side}_html" => source))
        inline = options.merge("#{side}_html_content" => markup)
        expect(normalize(Sghtmltopdf.render(html, **inline))).to eq(expected)

        chunks = []
        result = Sghtmltopdf.render(html, **inline) { |bytes| chunks << bytes }
        expect(result).to be_nil
        expect(normalize(chunks.join)).to eq(expected)

        expect(Sghtmltopdf.render_to_file(html, output, **inline)).to be_nil
        expect(normalize(File.binread(output))).to eq(expected)
      end
    end

    it "merges and disables configured #{side} content" do
      key = :"#{side}_html_content"
      without_overlay = normalize(Sghtmltopdf.render(html))
      Sghtmltopdf.configure { |config| config[key] = markup }
      expect(normalize(Sghtmltopdf.render(html, **options)))
        .to eq(normalize(Sghtmltopdf.render(html, **options, key => markup)))
      expect(normalize(Sghtmltopdf.render(html, key => nil))).to eq(without_overlay)
      expect(normalize(Sghtmltopdf.render(html, key => false))).to eq(without_overlay)
    end

    it "rejects conflicting #{side} input even across configuration and call options" do
      Sghtmltopdf.configure { |config| config[:"#{side}_html"] = "/missing.html" }
      expect { Sghtmltopdf.render(html, "#{side}_html_content" => markup) }
        .to raise_error(Sghtmltopdf::UsageError, /cannot be used with/)
      expect(Sghtmltopdf.render(html, "#{side}_html" => nil, "#{side}_html_content" => markup))
        .to start_with("%PDF-")
    end

    it "retains streaming restrictions for #{side} total-page placeholders" do
      expect { Sghtmltopdf.render(html, streaming: true, "#{side}_html_content" => markup) }
        .to raise_error(Sghtmltopdf::RenderError, /\[topage\]/)
      expect(Sghtmltopdf.render(html, streaming: true, "#{side}_html_content" => "<div>[page]</div>"))
        .to start_with("%PDF-")
    end
  end

  it "passes content through server delegation without interpreting it as a path" do
    FakeServer.run do |server|
      Sghtmltopdf.render(html, server_url: server.url, header_html_content: markup, footer_html_content: "")
      query = URI.decode_www_form(server.last_request.query).to_h
      expect(query["header-html-content"]).to eq(markup)
      expect(query["footer-html-content"]).to eq("")
    end
  end
end
