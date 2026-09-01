# frozen_string_literal: true

require "tmpdir"

RSpec.describe "Sghtmltopdf.render" do
  let(:html) { "<html><head><title>請求書</title></head><body><h1>見出し</h1><p>本文です。</p></body></html>" }

  after { Sghtmltopdf.reset_config! }

  it "returns the PDF bytes" do
    pdf = Sghtmltopdf.render(html)
    expect(pdf).to start_with("%PDF-")
    expect(pdf.encoding).to eq(Encoding::ASCII_8BIT)
    expect(pdf).to end_with("%%EOF")
  end

  it "reflects the options in the result" do
    # The paper size shows up in the MediaBox numbers (with the same digit count the byte length is unchanged).
    a4 = normalize(Sghtmltopdf.render(html, page_size: "A4"))
    a5 = normalize(Sghtmltopdf.render(html, page_size: "A5"))
    expect(a4).not_to eq(a5)
  end

  describe "exception classes" do
    # When Rust panics inside the native extension, leaving it to magnus gives Ruby's
    # `fatal`, which not even `rescue Exception` catches and which takes the worker down.
    # The extension catches it and converts to this class, so an application can catch it
    # with an ordinary `rescue` and treat it as one failed request.
    it "lets InternalError be caught by an ordinary rescue" do
      expect(Sghtmltopdf::InternalError.ancestors).to include(Sghtmltopdf::Error)
      expect(Sghtmltopdf::InternalError.ancestors).to include(StandardError)
    end

    it "makes every exception a descendant of Sghtmltopdf::Error" do
      %i[UsageError InputError RenderError InternalError].each do |name|
        klass = Sghtmltopdf.const_get(name)
        expect(klass.ancestors).to include(Sghtmltopdf::Error), "#{name} is not under Error"
        expect(klass.ancestors).to include(StandardError), "#{name} is not under StandardError"
      end
    end
  end

  describe "errors" do
    it "makes an unknown option a UsageError (decided by clap)" do
      expect { Sghtmltopdf.render(html, no_such_option: "x") }
        .to raise_error(Sghtmltopdf::UsageError, /--no-such-option/)
    end

    it "makes an unsupported option a UsageError with a reason" do
      expect { Sghtmltopdf.render(html, enable_javascript: true) }
        .to raise_error(Sghtmltopdf::UsageError, /is not supported/)
    end

    it "makes a malformed value a UsageError too" do
      expect { Sghtmltopdf.render(html, page_size: "Z9") }
        .to raise_error(Sghtmltopdf::UsageError)
    end

    it "makes them all inherit Sghtmltopdf::Error" do
      expect(Sghtmltopdf::UsageError.ancestors).to include(Sghtmltopdf::Error, StandardError)
      expect(Sghtmltopdf::InputError.ancestors).to include(Sghtmltopdf::Error)
      expect(Sghtmltopdf::RenderError.ancestors).to include(Sghtmltopdf::Error)
    end
  end

  describe "the global configuration" do
    it "makes a value set with configure the default" do
      Sghtmltopdf.configure { |c| c.page_size = "A5" }
      expect(normalize(Sghtmltopdf.render(html)))
        .to eq(normalize(Sghtmltopdf.render(html, page_size: "A5")))
    end

    it "lets a call-time option beat the global configuration" do
      Sghtmltopdf.configure { |c| c.page_size = "A5" }
      expect(normalize(Sghtmltopdf.render(html, page_size: "A4")))
        .to eq(normalize(Sghtmltopdf.render(html, page_size: "A4")))
      expect(normalize(Sghtmltopdf.render(html, page_size: "A4")))
        .not_to eq(normalize(Sghtmltopdf.render(html)))
    end
  end

  describe "thread safety" do
    it "gives the same result when called from several threads at once" do
      expected = normalize(Sghtmltopdf.render(html))
      results = 4.times.map { Thread.new { Sghtmltopdf.render(html) } }.map(&:value)
      expect(results.map { |pdf| normalize(pdf) }).to all(eq(expected))
    end
  end
end

RSpec.describe "Sghtmltopdf.render_to_file" do
  let(:html) { "<p>ファイルへ書き出す</p>" }

  around do |example|
    Dir.mktmpdir("sghtmltopdf-spec") { |dir| @dir = dir and example.run }
  end

  it "writes the PDF to a file" do
    path = File.join(@dir, "out.pdf")
    expect(Sghtmltopdf.render_to_file(html, path)).to be_nil
    expect(File.binread(path)).to start_with("%PDF-")
  end

  it "produces the same content as render" do
    path = File.join(@dir, "out.pdf")
    Sghtmltopdf.render_to_file(html, path)
    expect(normalize(File.binread(path))).to eq(normalize(Sghtmltopdf.render(html)))
  end

  it "raises InputError for a location it cannot write to" do
    expect { Sghtmltopdf.render_to_file(html, File.join(@dir, "no", "dir", "out.pdf")) }
      .to raise_error(Sghtmltopdf::InputError)
  end

  it "leaves no broken PDF when the rendering fails" do
    path = File.join(@dir, "out.pdf")
    expect { Sghtmltopdf.render_to_file(html, path, page_size: "Z9") }
      .to raise_error(Sghtmltopdf::Error)
    expect(File.exist?(path)).to be(false)
  end
end

# Confirm from the bytes that the CLI and the gem converge on the same execution path.
RSpec.describe "matching the CLI's output" do
  # The binary `cargo build --release` produces at the repository root.
  CLI_PATH = File.expand_path("../../../target/release/sghtmltopdf", __dir__)

  before do
    skip "no CLI binary (build it with cargo build --release): #{CLI_PATH}" unless File.executable?(CLI_PATH)
  end

  def render_with_cli(html, *args)
    require "open3"
    out, err, status = Open3.capture3(CLI_PATH, "-", "-o", "-", "-q", *args, stdin_data: html, binmode: true)
    raise "the CLI failed: #{err}" unless status.success?

    out
  end

  [
    ["the default options", [], {}],
    ["page size and margins", ["--page-size", "A4", "--margin-top", "20mm"], {page_size: "A4", margin_top: "20mm"}],
    ["grayscale", ["--grayscale"], {grayscale: true}],
    ["メタデータ", ["--title", "請求書", "--author", "わか"], {title: "請求書", author: "わか"}],
    ["no compression", ["--no-pdf-compression"], {no_pdf_compression: true}],
  ].each do |name, cli_args, gem_options|
    it "produces the same PDF as the CLI with #{name}" do
      html = "<html><head><title>t</title></head><body><h1>見出し</h1><p>本文です。</p></body></html>"
      expect(normalize(Sghtmltopdf.render(html, **gem_options)))
        .to eq(normalize(render_with_cli(html, *cli_args)))
    end
  end
end
