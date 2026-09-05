# frozen_string_literal: true

require "tmpdir"

# サーバモードへの委譲。
RSpec.describe "server_url" do
  let(:html) { "<h1>Invoice</h1>" }

  after { Sghtmltopdf.reset_config! }

  describe "リクエストの組み立て" do
    it "POST /pdf にHTMLをボディで送る" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url)

        expect(server.last_request.method).to eq("POST")
        expect(server.last_request.path).to eq("/pdf")
        expect(server.last_request.body).to eq(html)
      end
    end

    it "オプションをクエリ文字列にする(CLIのフラグ名と同じ)" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, page_size: "A4", margin_top: "20mm", toc: true)

        expect(server.last_request.query.split("&"))
          .to contain_exactly("page-size=A4", "margin-top=20mm", "toc")
      end
    end

    it "値はパーセントエンコードする" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, title: "請求書 2026")

        expect(server.last_request.query).to eq("title=#{URI.encode_www_form_component("請求書 2026")}")
      end
    end

    it "server_url自体やタイムアウトはクエリに出さない" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, server_read_timeout: 5, server_open_timeout: 1)

        expect(server.last_request.query).to eq("")
      end
    end

    it "偽の値は指定なしと同じ" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url, grayscale: false, toc: nil)

        expect(server.last_request.query).to eq("")
      end
    end

    it "グローバル設定でも指定できる" do
      FakeServer.run do |server|
        Sghtmltopdf.configure do |c|
          c.server_url = server.url
          c.page_size = "A5"
        end
        Sghtmltopdf.render(html)

        expect(server.last_request.query).to eq("page-size=A5")
      end
    end

    it "流し込まれた既定値はサーバへ送らない(サーバでは指定できないキーのため)" do
      FakeServer.run do |server|
        # Railtieが入れる既定値と同じ形。
        Sghtmltopdf.config.apply_defaults(base_url: "/app/public", allow_path: ["/app"])
        Sghtmltopdf.render(html, server_url: server.url, page_size: "A4")

        expect(server.last_request.query).to eq("page-size=A4")
      end
    end

    it "明示的に設定した値は送る(可否の判断はサーバに任せる)" do
      FakeServer.run do |server|
        Sghtmltopdf.configure { |c| c.base_url = "/app/public" }
        Sghtmltopdf.render(html, server_url: server.url)

        expect(server.last_request.query).to eq("base-url=#{URI.encode_www_form_component("/app/public")}")
      end
    end
  end

  describe "レスポンス" do
    it "200のボディをそのまま返す" do
      FakeServer.run { |server| expect(Sghtmltopdf.render(html, server_url: server.url)).to start_with("%PDF-") }
    end

    it "返り値はASCII-8BITにする" do
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
      it "#{status}は#{error}にして、サーバの文言をそのまま伝える" do
        FakeServer.run(->(_req) { [status, "サーバからの説明"] }) do |server|
          expect { Sghtmltopdf.render(html, server_url: server.url) }
            .to raise_error(error, /サーバからの説明/)
        end
      end
    end

    it "ServerErrorもSghtmltopdf::Errorを継承する" do
      expect(Sghtmltopdf::ServerError.ancestors).to include(Sghtmltopdf::Error, StandardError)
    end
  end

  describe "到達できないとき" do
    it "接続を拒否されたらServerErrorにする(ローカルへフォールバックしない)" do
      # 待ち受けていないポートを確実に得るため、開いた直後に閉じる。
      socket = TCPServer.new("127.0.0.1", 0)
      port = socket.addr[1]
      socket.close

      expect { Sghtmltopdf.render(html, server_url: "http://127.0.0.1:#{port}") }
        .to raise_error(Sghtmltopdf::ServerError, /接続に失敗しました/)
    end

    it "応答が遅ければread_timeoutで打ち切る" do
      FakeServer.run(->(_req) { sleep 5 }) do |server|
        expect { Sghtmltopdf.render(html, server_url: server.url, server_read_timeout: 0.2) }
          .to raise_error(Sghtmltopdf::ServerError, /タイムアウト/)
      end
    end

    it "http(s)以外のURLはArgumentErrorにする" do
      expect { Sghtmltopdf.render(html, server_url: "pdf.internal:8080") }
        .to raise_error(ArgumentError, /http\(s\)/)
    end
  end

  describe "チャンクごとの受け取り" do
    it "ブロックを渡すと?stream=1を付けて逐次受け取る" do
      FakeServer.run(->(_req) { [200, ["%PDF-1.7\n", "page1\n", "%%EOF"]] }) do |server|
        chunks = []
        result = Sghtmltopdf.render(html, server_url: server.url) { |bytes| chunks << bytes }

        expect(result).to be_nil
        expect(chunks.join).to eq("%PDF-1.7\npage1\n%%EOF")
        expect(server.last_request.query.split("&")).to include("stream=1")
      end
    end

    it "ブロックが無ければstream=1は付けない" do
      FakeServer.run do |server|
        Sghtmltopdf.render(html, server_url: server.url)

        expect(server.last_request.query).not_to include("stream")
      end
    end

    it "ローカル変換でも同じ形で受け取れる(詳細はchunk_spec.rb)" do
      chunks = []
      result = Sghtmltopdf.render(html) { |bytes| chunks << bytes }

      expect(result).to be_nil
      expect(chunks.join).to start_with("%PDF-")
    end
  end

  describe "render_to_file" do
    around { |example| Dir.mktmpdir("sghtmltopdf-server") { |dir| @dir = dir and example.run } }

    it "サーバの応答をファイルへ書き出す" do
      FakeServer.run do |server|
        path = File.join(@dir, "out.pdf")
        expect(Sghtmltopdf.render_to_file(html, path, server_url: server.url)).to be_nil
        expect(File.binread(path)).to start_with("%PDF-")
      end
    end

    it "失敗したら壊れたPDFを残さない" do
      FakeServer.run(->(_req) { [500, "レンダリングに失敗しました"] }) do |server|
        path = File.join(@dir, "out.pdf")
        expect { Sghtmltopdf.render_to_file(html, path, server_url: server.url) }
          .to raise_error(Sghtmltopdf::RenderError)
        expect(File.exist?(path)).to be(false)
        expect(Dir.children(@dir)).to be_empty
      end
    end

    it "書けない場所ならInputErrorにする" do
      FakeServer.run do |server|
        expect { Sghtmltopdf.render_to_file(html, File.join(@dir, "no", "dir", "out.pdf"), server_url: server.url) }
          .to raise_error(Sghtmltopdf::InputError)
      end
    end
  end
end

# 実際の`sghtmltopdf server`と繋いで、ローカル変換と同じPDFになることを確かめる。
RSpec.describe "実サーバとの結合" do
  CLI_BINARY = File.expand_path("../../../target/release/sghtmltopdf", __dir__)

  before(:all) do
    skip "CLIバイナリが無い(cargo build --release で作れる)" unless File.executable?(CLI_BINARY)

    require "open3"
    @stdin, @stdout, @wait = Open3.popen2(CLI_BINARY, "server", "--listen", "127.0.0.1:0")
    # 起動時に`listening on 127.0.0.1:<port>`を出す。
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

  it "ローカル変換と同じPDFになる" do
    remote = Sghtmltopdf.render(html, server_url: @server_url, page_size: "A4", margin_top: "20mm")
    local = Sghtmltopdf.render(html, page_size: "A4", margin_top: "20mm")

    expect(normalize(remote)).to eq(normalize(local))
  end

  it "ストリーミング受信でも同じPDFになる" do
    chunks = []
    Sghtmltopdf.render(html, server_url: @server_url) { |bytes| chunks << bytes }

    expect(normalize(chunks.join)).to eq(normalize(Sghtmltopdf.render(html)))
  end

  it "未知のオプションはUsageErrorになる(サーバの400)" do
    expect { Sghtmltopdf.render(html, server_url: @server_url, no_such_option: "x") }
      .to raise_error(Sghtmltopdf::UsageError, /no-such-option/)
  end

  it "サーバ起動時にしか指定できないオプションは理由付きのUsageErrorになる" do
    expect { Sghtmltopdf.render(html, server_url: @server_url, base_url: "/tmp") }
      .to raise_error(Sghtmltopdf::UsageError, /リクエストからは指定できません/)
  end

  it "healthzを叩いていないのに404にならない(パスは/pdf固定)" do
    expect(Sghtmltopdf.render(html, server_url: @server_url)).to start_with("%PDF-")
  end
end
