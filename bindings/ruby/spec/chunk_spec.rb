# frozen_string_literal: true

# Chunked output from a local conversion (the native extension).
RSpec.describe "a block-taking render (local conversion)" do
  # HTML that makes several pages. It is written to the Sink as each page settles, so the
  # chunks come in several calls.
  def multipage_html(pages: 12)
    body = Array.new(pages) do |i|
      "<div style=\"break-after: page\"><h1>Page #{i + 1}</h1>" \
        "<p>#{"本文です。" * 40}</p></div>"
    end.join
    "<html><head><title>chunks</title></head><body>#{body}</body></html>"
  end

  after { Sghtmltopdf.reset_config! }

  it "yields several times, page by page as they settle" do
    chunks = []
    Sghtmltopdf.render(multipage_html, chunk_size: 1024) { |bytes| chunks << bytes }

    expect(chunks.size).to be > 1
    expect(chunks.first).to start_with("%PDF-")
    expect(chunks.last).to end_with("%%EOF")
  end

  it "gives the same PDF as a one-shot render when joined" do
    html = multipage_html
    chunks = []
    Sghtmltopdf.render(html, chunk_size: 1024) { |bytes| chunks << bytes }

    expect(normalize(chunks.join)).to eq(normalize(Sghtmltopdf.render(html)))
  end

  it "returns nil" do
    expect(Sghtmltopdf.render("<p>x</p>") { |_| }).to be_nil
  end

  it "hands over ASCII-8BIT chunks" do
    encodings = []
    Sghtmltopdf.render(multipage_html, chunk_size: 1024) { |bytes| encodings << bytes.encoding }

    expect(encodings.uniq).to eq([Encoding::ASCII_8BIT])
  end

  describe "chunk_size" do
    it "gives more chunks when made smaller" do
      html = multipage_html
      few = 0
      many = 0
      Sghtmltopdf.render(html, chunk_size: 64 * 1024) { |_| few += 1 }
      Sghtmltopdf.render(html, chunk_size: 512) { |_| many += 1 }

      expect(many).to be > few
    end

    it "defaults to 64KiB" do
      expect(Sghtmltopdf::DEFAULT_CHUNK_SIZE).to eq(64 * 1024)
    end

    it "can be set through the global configuration too" do
      Sghtmltopdf.configure { |c| c.chunk_size = 512 }
      chunks = 0
      Sghtmltopdf.render(multipage_html) { |_| chunks += 1 }

      expect(chunks).to be > 1
    end

    it "is not passed as a conversion option (clap does not know the key)" do
      expect { Sghtmltopdf.render("<p>x</p>", chunk_size: 512) { |_| } }.not_to raise_error
    end
  end

  describe "when the block is interrupted" do
    it "propagates the thrown exception unchanged" do
      expect { Sghtmltopdf.render(multipage_html, chunk_size: 512) { |_| raise ArgumentError, "stop" } }
        .to raise_error(ArgumentError, "stop")
    end

    it "does not break the VM when the GC runs after an exception" do
      10.times do
        Sghtmltopdf.render(multipage_html, chunk_size: 512) { |_| raise "stop" }
      rescue RuntimeError
        nil
      end
      GC.start

      # If it were broken we would have crashed by now. It also confirms conversion still works.
      expect(Sghtmltopdf.render("<p>ok</p>")).to start_with("%PDF-")
    end

    it "propagates an exception correctly under GC.stress too" do
      # A Ruby String is created per chunk, so the GC runs here.
      # Mishandling the block or the values on the stack would crash.
      GC.stress = true
      begin
        expect { Sghtmltopdf.render("<p>x</p>", chunk_size: 512) { |_| raise IndexError, "stop" } }
          .to raise_error(IndexError, "stop")
      ensure
        GC.stress = false
      end
    end

    it "can still convert normally after an interruption" do
      Sghtmltopdf.render(multipage_html, chunk_size: 512) { |_| raise "stop" }
    rescue RuntimeError
      expect(Sghtmltopdf.render("<p>ok</p>")).to start_with("%PDF-")
    end
  end

  describe "other threads" do
    it "has the GVL released during rendering" do
      counter = 0
      # A busy loop would fight over the GVL and slow the rendering itself, so it sleeps a
      # little as it goes and only checks whether the other thread made progress.
      worker = Thread.new do
        loop do
          counter += 1
          sleep 0.001
        end
      end
      Sghtmltopdf.render(multipage_html, chunk_size: 512) { |_| }
      worker.kill
      worker.join

      expect(counter).to be > 0
    end
  end

  # A side effect: calling the block is a Ruby method call, so any pending interrupt is
  # handled there.
  describe "interruption" do
    # The conversion's own speed varies several-fold by machine, so "still mid-conversion at
    # the point we try to stop it" is arranged without relying on the running time. Sleeping
    # a little per chunk makes the total time the sum of the sleeps.
    CHUNK_SLEEP = 0.005

    it "lets Thread#kill take effect at a chunk boundary" do
      first_chunk = Queue.new
      thread = Thread.new do
        Sghtmltopdf.render(multipage_html(pages: 120), chunk_size: 512) do |_|
          first_chunk << true
          sleep CHUNK_SLEEP
        end
      end
      # Wait for the first chunk before stopping it.
      first_chunk.pop
      thread.kill

      expect(thread.join(10)).to eq(thread)
      expect(thread.alive?).to be(false)
      # The VM is not broken.
      expect(Sghtmltopdf.render("<p>ok</p>")).to start_with("%PDF-")
    end

    it "lets Timeout.timeout take effect" do
      require "timeout"

      expect {
        Timeout.timeout(0.1) do
          Sghtmltopdf.render(multipage_html(pages: 120), chunk_size: 512) { |_| sleep CHUNK_SLEEP }
        end
      }.to raise_error(Timeout::Error)

      expect(Sghtmltopdf.render("<p>ok</p>")).to start_with("%PDF-")
    end
  end
end
