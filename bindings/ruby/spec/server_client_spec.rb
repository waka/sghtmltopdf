# frozen_string_literal: true

require "tmpdir"

# Delegating to server mode.
RSpec.describe "server_url" do
  let(:html) { "<h1>Invoice</h1>" }

  after { Sghtmltopdf.reset_config! }

  describe "building the request" do
    it "sends the HTML as the body of POST /pdf" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url)

        expect(server.last_request.method).to eq("POST")
        expect(server.last_request.path).to eq("/pdf")
        expect(server.last_request.body).to eq(html)
      end
    end

    it "turns the options into a query string (the same flag names as the CLI)" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, page_size: "A4", margin_top: "20mm", toc: true)

        expect(server.last_request.query.split("&"))
          .to contain_exactly("page-size=A4", "margin-top=20mm", "toc")
      end
    end

    it "percent-encodes the values" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, title: "請求書 2026")

        expect(server.last_request.query).to eq("title=#{URI.encode_www_form_component("請求書 2026")}")
      end
    end

    it "keeps server_url itself and the timeout out of the query" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, server_read_timeout: 5, server_open_timeout: 1)

        expect(server.last_request.query).to eq("")
      end
    end

    it "treats a false value the same as not given" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, grayscale: false, toc: nil)

        expect(server.last_request.query).to eq("")
      end
    end

    it "can be set through the global configuration too" do
      FakeServer.run do |server|
        Sghtmltopdf.configure do |c|
          c.server_url = server.url
          c.page_size = "A5"
        end
        Sghtmltopdf.render(html)

        expect(server.last_request.query).to eq("page-size=A5")
      end
    end

    it "does not send the injected defaults to the server (they cannot be set there)" do
      FakeServer.run do |server|
        # The same shape as the defaults the Railtie injects.
        Sghtmltopdf.config.apply_defaults(base_url: "/app/public", allow: ["/app"])
        Sghtmltopdf.render(html, server_url: server.url, page_size: "A4")

        expect(server.last_request.query).to eq("page-size=A4")
      end
    end

    it "sends an explicitly set value (leaving the decision to the server)" do
      FakeServer.run do |server|
        Sghtmltopdf.configure { |c| c.base_url = "/app/public" }
        Sghtmltopdf.render(html, server_url: server.url)

        expect(server.last_request.query).to eq("base-url=#{URI.encode_www_form_component("/app/public")}")
      end
    end
  end

  describe "the response" do
    it "returns a 200's body unchanged" do
      FakeServer.run { |server| expect(Sghtmltopdf.render(html, server_url: server.url)).to start_with("%PDF-") }
    end

    it "makes the return value ASCII-8BIT" do
      FakeServer.run do |server|
        expect(Sghtmltopdf.render(html, server_url: server.url).encoding).to eq(Encoding::ASCII_8BIT)
      end
    end

    {
      400 => Sghtmltopdf::UsageError,
      413 => Sghtmltopdf::InputError,
      500 => Sghtmltopdf::RenderError,
      503 => Sghtmltopdf::ServerError,
      504 => Sghtmltopdf::ServerError,
      404 => Sghtmltopdf::ServerError,
    }.each do |status, error|
      it "turns #{status} into #{error} and passes the server's wording through" do
        FakeServer.run(->(_req) { [status, "an explanation from the server"] }) do |server|
          expect { Sghtmltopdf.render(html, server_url: server.url) }
            .to raise_error(error, /an explanation from the server/)
        end
      end
    end

    it "makes ServerError inherit Sghtmltopdf::Error too" do
      expect(Sghtmltopdf::ServerError.ancestors).to include(Sghtmltopdf::Error, StandardError)
    end
  end

  describe "when it cannot be reached" do
    it "makes a refused connection a ServerError (it does not fall back to local)" do
      # Open and immediately close, to be sure of a port nothing is listening on.
      socket = TCPServer.new("127.0.0.1", 0)
      port = socket.addr[1]
      socket.close

      expect { Sghtmltopdf.render(html, server_url: "http://127.0.0.1:#{port}") }
        .to raise_error(Sghtmltopdf::ServerError, /the connection to .* failed/)
    end

    it "gives up through read_timeout when the response is slow" do
      FakeServer.run(->(_req) { sleep 5 }) do |server|
        expect { Sghtmltopdf.render(html, server_url: server.url, server_read_timeout: 0.2) }
          .to raise_error(Sghtmltopdf::ServerError, /timed out/)
      end
    end

    it "makes a non-http(s) URL an ArgumentError" do
      expect { Sghtmltopdf.render(html, server_url: "pdf.internal:8080") }
        .to raise_error(ArgumentError, /http\(s\)/)
    end
  end

  describe "receiving chunk by chunk" do
    it "adds ?stream=1 and receives incrementally when given a block" do
      FakeServer.run(->(_req) { [200, ["%PDF-1.7\n", "page1\n", "%%EOF"]] }) do |server|
        chunks = []
        result = Sghtmltopdf.render(html, server_url: server.url) { |bytes| chunks << bytes }

        expect(result).to be_nil
        expect(chunks.join).to eq("%PDF-1.7\npage1\n%%EOF")
        expect(server.last_request.query.split("&")).to include("stream=1")
      end
    end

    it "does not add stream=1 with no block" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url)

        expect(server.last_request.query).not_to include("stream")
      end
    end

    it "can be received the same way from a local conversion too (see chunk_spec.rb)" do
      chunks = []
      result = Sghtmltopdf.render(html) { |bytes| chunks << bytes }

      expect(result).to be_nil
      expect(chunks.join).to start_with("%PDF-")
    end
  end

  describe "render_to_file" do
    around { |example| Dir.mktmpdir("sghtmltopdf-server") { |dir| @dir = dir and example.run } }

    it "writes the server's response to a file" do
      FakeServer.run do |server|
        path = File.join(@dir, "out.pdf")
        expect(Sghtmltopdf.render_to_file(html, path, server_url: server.url)).to be_nil
        expect(File.binread(path)).to start_with("%PDF-")
      end
    end

    it "leaves no broken PDF on failure" do
      FakeServer.run(->(_req) { [500, "the rendering failed"] }) do |server|
        path = File.join(@dir, "out.pdf")
        expect { Sghtmltopdf.render_to_file(html, path, server_url: server.url) }
          .to raise_error(Sghtmltopdf::RenderError)
        expect(File.exist?(path)).to be(false)
        expect(Dir.children(@dir)).to be_empty
      end
    end

    it "raises InputError for a location it cannot write to" do
      FakeServer.run do |server|
        expect { Sghtmltopdf.render_to_file(html, File.join(@dir, "no", "dir", "out.pdf"), server_url: server.url) }
          .to raise_error(Sghtmltopdf::InputError)
      end
    end
  end
end

# Connect to a real `sghtmltopdf server` and confirm it gives the same PDF as a local conversion.
RSpec.describe "integration with a real server" do
  CLI_BINARY = File.expand_path("../../../target/release/sghtmltopdf", __dir__)

  before(:all) do
    skip "no CLI binary (build it with cargo build --release)" unless File.executable?(CLI_BINARY)

    require "open3"
    @stdin, @stdout, @wait = Open3.popen2(CLI_BINARY, "server", "--listen", "127.0.0.1:0")
    # It prints `listening on 127.0.0.1:<port>` on startup.
    line = @stdout.gets
    @server_url = "http://#{line[/listening on (\S+)/, 1]}"
  end

  after(:all) do
    next unless @wait

    Process.kill("TERM", @wait.pid)
    @wait.join
    [@stdin, @stdout].each { |io| io.close unless io.closed? }
  end

  after { Sghtmltopdf.reset_config! }

  let(:html) { "<html><head><title>t</title></head><body><h1>見出し</h1><p>本文です。</p></body></html>" }

  it "gives the same PDF as a local conversion" do
    remote = Sghtmltopdf.render(html, server_url: @server_url, page_size: "A4", margin_top: "20mm")
    local = Sghtmltopdf.render(html, page_size: "A4", margin_top: "20mm")

    expect(normalize(remote)).to eq(normalize(local))
  end

  it "gives the same PDF when received as a stream too" do
    chunks = []
    Sghtmltopdf.render(html, server_url: @server_url) { |bytes| chunks << bytes }

    expect(normalize(chunks.join)).to eq(normalize(Sghtmltopdf.render(html)))
  end

  it "makes an unknown option a UsageError (the server's 400)" do
    expect { Sghtmltopdf.render(html, server_url: @server_url, no_such_option: "x") }
      .to raise_error(Sghtmltopdf::UsageError, /no-such-option/)
  end

  it "makes an option that can only be set at server startup a UsageError with a reason" do
    expect { Sghtmltopdf.render(html, server_url: @server_url, base_url: "/tmp") }
      .to raise_error(Sghtmltopdf::UsageError, /cannot be set per request/)
  end

  it "does not 404 without hitting healthz (the path is always /pdf)" do
    expect(Sghtmltopdf.render(html, server_url: @server_url)).to start_with("%PDF-")
  end
end
