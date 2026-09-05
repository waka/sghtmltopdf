# frozen_string_literal: true

RSpec.describe Sghtmltopdf::Options do
  # argvの先頭は常に固定なので、比較しやすいよう取り除く。
  def argv(options)
    described_class.to_argv(options).drop(Sghtmltopdf::Options::ARGV_PREFIX.size)
  end

  it "入力と出力に標準ストリームを置いた先頭を必ず付ける" do
    expect(described_class.to_argv({})).to eq(["sghtmltopdf", "-", "--output", "-"])
  end

  describe "キーの変換" do
    it "アンダースコアをハイフンにする" do
      expect(argv(page_size: "A4")).to eq(["--page-size", "A4"])
    end

    it "文字列のキーも受ける" do
      expect(argv("page-size" => "A4")).to eq(["--page-size", "A4"])
    end
  end

  describe "値の変換" do
    it "trueはフラグだけにする" do
      expect(argv(grayscale: true)).to eq(["--grayscale"])
    end

    it "falseは指定なしと同じにする" do
      expect(argv(grayscale: false)).to eq([])
    end

    it "nilは指定なしと同じにする" do
      expect(argv(title: nil)).to eq([])
    end

    it "数値はto_sする" do
      expect(argv(dpi: 300)).to eq(["--dpi", "300"])
      expect(argv(zoom: 1.5)).to eq(["--zoom", "1.5"])
    end

    it "Pathnameのようなオブジェクトもto_sする" do
      require "pathname"
      expect(argv(user_style_sheet: Pathname.new("/tmp/a.css")))
        .to eq(["--user-style-sheet", "/tmp/a.css"])
    end

    it "配列は同じオプションの繰り返しにする" do
      expect(argv(allow_path: ["/a", "/b"])).to eq(["--allow-path", "/a", "--allow-path", "/b"])
    end

    it "配列の中のtrue/falseにも同じ規則を使う" do
      expect(argv(allow_path: ["/a", nil, "/b"])).to eq(["--allow-path", "/a", "--allow-path", "/b"])
    end

    # `--allow`はwkhtmltopdf互換の別名。argvでは正規名に寄せる。
    it "別名のキーは正規名のフラグになる" do
      expect(argv(allow: ["/a"])).to eq(["--allow-path", "/a"])
    end

    it "font以外にHashを渡すとエラーにする" do
      expect { argv(page_size: {a: 1}) }.to raise_error(ArgumentError, /Hashは渡せません/)
    end

    it "wicked_pdf形式の入れ子は平坦なキーを案内する" do
      expect { argv(margin: {top: 10}) }
        .to raise_error(ArgumentError, /margin_top/)
    end
  end

  describe "複数オプション" do
    it "渡された順に並べる" do
      expect(argv(page_size: "A4", margin_top: "20mm", grayscale: true))
        .to eq(["--page-size", "A4", "--margin-top", "20mm", "--grayscale"])
    end
  end

  describe "font(位置依存)" do
    it "文字列ひとつを--fontにする" do
      expect(argv(font: "a.ttf")).to eq(["--font", "a.ttf"])
    end

    it "pathとindexのHashではindexを直後に置く" do
      expect(argv(font: {path: "a.ttc", index: 1}))
        .to eq(["--font", "a.ttc", "--font-index", "1"])
    end

    it "配列では各fontの直後にそのindexを置く" do
      # `--font-index`は「手前にある最後の--font」に対応付けられるため、
      # この並び順でなければ別のフォントへ適用されてしまう。
      expect(argv(font: ["a.ttf", {path: "b.ttc", index: 2}]))
        .to eq(["--font", "a.ttf", "--font", "b.ttc", "--font-index", "2"])
    end

    it "index: 0も省略しない" do
      expect(argv(font: {path: "a.ttc", index: 0}))
        .to eq(["--font", "a.ttc", "--font-index", "0"])
    end

    it "文字列キーのHashも受ける" do
      expect(argv(font: {"path" => "a.ttc", "index" => 3}))
        .to eq(["--font", "a.ttc", "--font-index", "3"])
    end

    it "pathが無いHashはエラーにする" do
      expect { argv(font: {index: 1}) }.to raise_error(ArgumentError, /pathが必要/)
    end

    it "gothic_fontなど他のフォントオプションは通常の変換にする" do
      expect(argv(gothic_font: "g.ttf", gothic_font_index: 1))
        .to eq(["--gothic-font", "g.ttf", "--gothic-font-index", "1"])
    end
  end

  describe "妥当性検査をしないこと" do
    it "未知のオプションもそのままargvにする(判定はclapに任せる)" do
      expect(argv(no_such_option: "x")).to eq(["--no-such-option", "x"])
    end
  end
end
