# frozen_string_literal: true

require "tmpdir"

RSpec.describe "Sghtmltopdf.render" do
  let(:html) { "<html><head><title>請求書</title></head><body><h1>見出し</h1><p>本文です。</p></body></html>" }

  after { Sghtmltopdf.reset_config! }

  it "PDFのバイト列を返す" do
    pdf = Sghtmltopdf.render(html)
    expect(pdf).to start_with("%PDF-")
    expect(pdf.encoding).to eq(Encoding::ASCII_8BIT)
    expect(pdf).to end_with("%%EOF")
  end

  it "オプションが結果に反映される" do
    # 用紙サイズはMediaBoxの数値に出る(桁数が同じだとバイト長は変わらない)。
    a4 = normalize(Sghtmltopdf.render(html, page_size: "A4"))
    a5 = normalize(Sghtmltopdf.render(html, page_size: "A5"))
    expect(a4).not_to eq(a5)
  end

  describe "例外クラス" do
    # ネイティブ拡張の中でRustがパニックした場合、magnusに任せるとRubyの
    # `fatal`になり、`rescue Exception`でも捕まえられずワーカーごと落ちる。
    # 拡張側で捕まえてこのクラスへ変換しているので、アプリが普通の`rescue`で
    # 受けて1リクエストぶんの失敗として扱える。
    it "InternalErrorは通常のrescueで捕まえられる" do
      expect(Sghtmltopdf::InternalError.ancestors).to include(Sghtmltopdf::Error)
      expect(Sghtmltopdf::InternalError.ancestors).to include(StandardError)
    end

    it "すべての例外がSghtmltopdf::Errorの子孫になっている" do
      %i[UsageError InputError RenderError InternalError].each do |name|
        klass = Sghtmltopdf.const_get(name)
        expect(klass.ancestors).to include(Sghtmltopdf::Error), "#{name}がError配下でない"
        expect(klass.ancestors).to include(StandardError), "#{name}がStandardError配下でない"
      end
    end
  end

  describe "エラー" do
    it "未知のオプションはUsageErrorにする(判定はclap側)" do
      expect { Sghtmltopdf.render(html, no_such_option: "x") }
        .to raise_error(Sghtmltopdf::UsageError, /--no-such-option/)
    end

    it "非対応オプションは理由付きのUsageErrorにする" do
      expect { Sghtmltopdf.render(html, enable_javascript: true) }
        .to raise_error(Sghtmltopdf::UsageError, /対応していません/)
    end

    it "値の形式エラーもUsageErrorにする" do
      expect { Sghtmltopdf.render(html, page_size: "Z9") }
        .to raise_error(Sghtmltopdf::UsageError)
    end

    it "すべてSghtmltopdf::Errorを継承する" do
      expect(Sghtmltopdf::UsageError.ancestors).to include(Sghtmltopdf::Error, StandardError)
      expect(Sghtmltopdf::InputError.ancestors).to include(Sghtmltopdf::Error)
      expect(Sghtmltopdf::RenderError.ancestors).to include(Sghtmltopdf::Error)
    end
  end

  describe "グローバル設定" do
    it "configureで設定した値が既定になる" do
      Sghtmltopdf.configure { |c| c.page_size = "A5" }
      expect(normalize(Sghtmltopdf.render(html)))
        .to eq(normalize(Sghtmltopdf.render(html, page_size: "A5")))
    end

    it "呼び出し時のオプションがグローバル設定に勝つ" do
      Sghtmltopdf.configure { |c| c.page_size = "A5" }
      expect(normalize(Sghtmltopdf.render(html, page_size: "A4")))
        .to eq(normalize(Sghtmltopdf.render(html, page_size: "A4")))
      expect(normalize(Sghtmltopdf.render(html, page_size: "A4")))
        .not_to eq(normalize(Sghtmltopdf.render(html)))
    end

    # `allow`は`allow_path`の別名。2つのキーのまま持ち回ると、既定が一方で
    # 呼び出し時の指定がもう一方のときにマージが上書きにならず、両方が
    # argvへ出てしまう(同じフラグの繰り返しは「合併」の意味になる)。
    it "別名のキーは正規名へ寄せられ、既定を上書きできる" do
      Sghtmltopdf.configure { |c| c.allow_path = ["/nonexistent-default"] }

      expect(Sghtmltopdf.config[:allow]).to eq(["/nonexistent-default"])

      Sghtmltopdf.configure { |c| c.allow = ["/nonexistent-other"] }

      expect(Sghtmltopdf.config[:allow_path]).to eq(["/nonexistent-other"])
      expect(Sghtmltopdf.config.to_h.keys).to include(:allow_path)
      expect(Sghtmltopdf.config.to_h.keys).not_to include(:allow)
    end
  end

  describe "スレッド安全性" do
    it "複数スレッドから同時に呼んでも同じ結果になる" do
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

  it "PDFをファイルへ書き出す" do
    path = File.join(@dir, "out.pdf")
    expect(Sghtmltopdf.render_to_file(html, path)).to be_nil
    expect(File.binread(path)).to start_with("%PDF-")
  end

  it "renderと同じ内容になる" do
    path = File.join(@dir, "out.pdf")
    Sghtmltopdf.render_to_file(html, path)
    expect(normalize(File.binread(path))).to eq(normalize(Sghtmltopdf.render(html)))
  end

  it "書けない場所ならInputErrorにする" do
    expect { Sghtmltopdf.render_to_file(html, File.join(@dir, "no", "dir", "out.pdf")) }
      .to raise_error(Sghtmltopdf::InputError)
  end

  it "レンダリングに失敗したら壊れたPDFを残さない" do
    path = File.join(@dir, "out.pdf")
    expect { Sghtmltopdf.render_to_file(html, path, page_size: "Z9") }
      .to raise_error(Sghtmltopdf::Error)
    expect(File.exist?(path)).to be(false)
  end
end

# CLIとgemが同じ実行経路に合流していることをバイト列で確かめる。
RSpec.describe "CLIとの出力一致" do
  # リポジトリルートの`cargo build --release`で作られるバイナリ。
  CLI_PATH = File.expand_path("../../../target/release/sghtmltopdf", __dir__)

  before do
    skip "CLIバイナリが無い(cargo build --release で作れる): #{CLI_PATH}" unless File.executable?(CLI_PATH)
  end

  def render_with_cli(html, *args)
    require "open3"
    out, err, status = Open3.capture3(CLI_PATH, "-", "-o", "-", "-q", *args, stdin_data: html, binmode: true)
    raise "CLIが失敗しました: #{err}" unless status.success?

    out
  end

  [
    ["既定のオプション", [], {}],
    ["ページサイズと余白", ["--page-size", "A4", "--margin-top", "20mm"], {page_size: "A4", margin_top: "20mm"}],
    ["グレースケール", ["--grayscale"], {grayscale: true}],
    ["メタデータ", ["--title", "請求書", "--author", "わか"], {title: "請求書", author: "わか"}],
    ["圧縮なし", ["--no-pdf-compression"], {no_pdf_compression: true}],
  ].each do |name, cli_args, gem_options|
    it "#{name}でCLIと同じPDFを出す" do
      html = "<html><head><title>t</title></head><body><h1>見出し</h1><p>本文です。</p></body></html>"
      expect(normalize(Sghtmltopdf.render(html, **gem_options)))
        .to eq(normalize(render_with_cli(html, *cli_args)))
    end
  end
end
