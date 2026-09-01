# frozen_string_literal: true

# Using the engine's streaming mode (`streaming: true`) from the gem.
#
# Where `chunk_spec.rb`'s block-taking `render` hands over an assembled PDF page by page as
# they settle, this is the mode that "settles pages while reading the HTML and releases that
# page's memory", where the memory ceiling starts to matter.
RSpec.describe "streaming: true" do
  # A paragraph-heavy document. The heights are fixed so the page count does not depend on
  # the environment's fonts.
  def paragraph_html(count: 3_000)
    body = Array.new(count) { |i| "<p>段落 #{i} 本文です。</p>" }.join
    "<html><head><style>p { height: 60px; margin: 0; }</style></head>" \
      "<body>#{body}</body></html>"
  end

  # Count the page objects of a PDF written uncompressed.
  def page_count(pdf)
    pdf.scan(%r{/Type\s*/Page[^s]}).size
  end

  after { Sghtmltopdf.reset_config! }

  it "returns the PDF bytes even with no block" do
    pdf = Sghtmltopdf.render(paragraph_html(count: 100), streaming: true)

    expect(pdf).to start_with("%PDF-")
    expect(pdf).to end_with("%%EOF")
  end

  it "yields several times, page by page as they settle" do
    chunks = []
    Sghtmltopdf.render(paragraph_html, streaming: true, chunk_size: 1024) { |bytes| chunks << bytes }

    expect(chunks.size).to be > 1
    expect(chunks.first).to start_with("%PDF-")
    expect(chunks.last).to end_with("%%EOF")
  end

  it "gives the same page count as the normal mode" do
    html = paragraph_html
    batch = Sghtmltopdf.render(html, no_pdf_compression: true)
    streamed = +"".b
    Sghtmltopdf.render(html, streaming: true, no_pdf_compression: true) { |bytes| streamed << bytes }

    expect(page_count(streamed)).to eq(page_count(batch))
  end

  # A forced page break is a relationship between top-level elements, so in streaming mode,
  # which processes one element at a time, the paginator has to handle it.
  it "gives the same page breaks from break-after as the normal mode" do
    body = Array.new(10) { |i| "<div style=\"break-after: page\">ページ #{i}</div>" }.join
    html = "<html><body>#{body}</body></html>"
    batch = Sghtmltopdf.render(html, no_pdf_compression: true)
    streamed = +"".b
    Sghtmltopdf.render(html, streaming: true, no_pdf_compression: true) { |bytes| streamed << bytes }

    expect(page_count(batch)).to eq(10)
    expect(page_count(streamed)).to eq(page_count(batch))
  end

  it "works with render_to_file too" do
    require "tmpdir"

    Dir.mktmpdir("sghtmltopdf-streaming") do |dir|
      path = File.join(dir, "out.pdf")
      Sghtmltopdf.render_to_file(paragraph_html(count: 100), path, streaming: true)

      expect(File.binread(path)).to start_with("%PDF-")
    end
  end

  # Anything that cannot be decided without seeing the whole document is an error rather than a silent change of result.
  describe "the streaming mode constraints" do
    it "makes --toc an error" do
      expect { Sghtmltopdf.render(paragraph_html(count: 10), streaming: true, toc: true) }
        .to raise_error(Sghtmltopdf::RenderError, /toc/)
    end

    it "gives the same error with a block too" do
      expect { Sghtmltopdf.render(paragraph_html(count: 10), streaming: true, toc: true) { |_| } }
        .to raise_error(Sghtmltopdf::RenderError, /toc/)
    end
  end

  # The whole point of streaming mode. Without releasing pages as they settle it would use as
  # much memory as the normal mode.
  describe "peak memory" do
    # The peak RSS (MB) added by the conversion.
    #
    # The peak RSS (VmHWM) never falls, so a child process is started per condition.
    # Ruby itself and the HTML string weigh the same in both modes, so the difference before
    # and after the conversion is used, leaving only the difference between the modes.
    def render_growth_mb(options)
      script = <<~RUBY
        require "sghtmltopdf"
        body = Array.new(40_000) { |i| "<p>paragraph \#{i} body text.</p>" }.join
        html = "<html><head><style>p { height: 60px; margin: 0; }</style></head>" \\
          "<body>\#{body}</body></html>"
        def peak_kib = File.read("/proc/self/status")[/VmHWM:\\s+(\\d+) kB/, 1].to_i
        before = peak_kib
        Sghtmltopdf.render(html, **#{options.inspect}) { |_| }
        puts peak_kib - before
      RUBY
      lib = File.expand_path("../lib", __dir__)
      output = IO.popen([RbConfig.ruby, "-I", lib, "-e", script], &:read)
      Integer(output.strip) / 1024.0
    end

    before do
      skip "an environment where VmHWM cannot be read" unless File.exist?("/proc/self/status")
    end

    it "is smaller than the normal mode" do
      batch = render_growth_mb({})
      streaming = render_growth_mb({streaming: true})

      # Measured at 105MB against 44MB (0.42x) with 40,000 elements. Failing to release pages
      # would bring it level with the normal mode, so a loose threshold allows for environment differences.
      expect(streaming).to be < batch * 0.6
    end
  end
end
