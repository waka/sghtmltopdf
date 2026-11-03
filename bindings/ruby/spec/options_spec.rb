# frozen_string_literal: true

RSpec.describe Sghtmltopdf::Options do
  def argv(options)
    described_class.to_argv(options)
  end

  it "adds nothing when there are no options" do
    expect(described_class.to_argv({})).to eq([])
  end

  describe "key conversion" do
    it "turns underscores into hyphens" do
      expect(argv(page_size: "A4")).to eq(["--page-size", "A4"])
    end

    it "accepts string keys too" do
      expect(argv("page-size" => "A4")).to eq(["--page-size", "A4"])
    end
  end

  describe "value conversion" do
    it "turns true into the flag alone" do
      expect(argv(grayscale: true)).to eq(["--grayscale"])
    end

    it "treats false the same as not given" do
      expect(argv(grayscale: false)).to eq([])
    end

    it "treats nil the same as not given" do
      expect(argv(title: nil)).to eq([])
    end

    it "calls to_s on a number" do
      expect(argv(dpi: 300)).to eq(["--dpi", "300"])
      expect(argv(zoom: 1.5)).to eq(["--zoom", "1.5"])
    end

    it "calls to_s on an object such as a Pathname too" do
      require "pathname"
      expect(argv(user_style_sheet: Pathname.new("/tmp/a.css")))
        .to eq(["--user-style-sheet", "/tmp/a.css"])
    end

    it "converts a Time to ISO 8601 in UTC" do
      time = Time.new(2026, 10, 22, 10, 30, 0, "+02:00")
      expect(argv(creation_date: time)).to eq(["--creation-date", "2026-10-22T08:30:00Z"])
    end

    it "converts a Time without anything else loaded (Time#iso8601 needs `require \"time\"` before Ruby 3.4)" do
      require "open3"
      script = 'require "sghtmltopdf/options"; ' \
               'print Sghtmltopdf::Options.to_argv(creation_date: Time.at(0)).last'
      out, err, status = Open3.capture3(
        {"RUBYOPT" => nil}, RbConfig.ruby, "--disable-gems", "-I", File.expand_path("../lib", __dir__), "-e", script
      )
      expect(status).to be_success, err
      expect(out).to eq("1970-01-01T00:00:00Z")
    end

    it "converts a DateTime to ISO 8601 in UTC" do
      datetime = DateTime.new(2026, 10, 22, 10, 30, 0, "-05:00")
      expect(argv(creation_date: datetime)).to eq(["--creation-date", "2026-10-22T15:30:00Z"])
    end

    it "passes a Date as a calendar date and a String as it is" do
      expect(argv(creation_date: Date.new(2026, 10, 22))).to eq(["--creation-date", "2026-10-22"])
      expect(argv(creation_date: "2026-10-22T10:30:00+02:00"))
        .to eq(["--creation-date", "2026-10-22T10:30:00+02:00"])
    end

    it "sends the converted date in the server query too" do
      time = Time.new(2026, 10, 22, 10, 30, 0, "+02:00")
      expect(described_class.to_query(creator: "Acme Reports", creation_date: time))
        .to eq("creator=Acme+Reports&creation-date=2026-10-22T08%3A30%3A00Z")
    end

    it "turns an array into a repeated option" do
      expect(argv(allow_path: ["/a", "/b"])).to eq(["--allow-path", "/a", "--allow-path", "/b"])
    end

    it "applies the same rules to true/false inside an array" do
      expect(argv(allow_path: ["/a", nil, "/b"])).to eq(["--allow-path", "/a", "--allow-path", "/b"])
    end

    # `--allow` is a wkhtmltopdf-compatible alias. argv normalizes it to the canonical name.
    it "turns an alias key into the canonical flag" do
      expect(argv(allow: ["/a"])).to eq(["--allow-path", "/a"])
    end

    it "raises for a Hash given to anything but font" do
      expect { argv(page_size: {a: 1}) }.to raise_error(ArgumentError, /a Hash cannot be passed/)
    end

    it "points at flat keys for wicked_pdf-style nesting" do
      expect { argv(margin: {top: 10}) }
        .to raise_error(ArgumentError, /margin_top/)
    end
  end

  describe "several options" do
    it "lists them in the order given" do
      expect(argv(page_size: "A4", margin_top: "20mm", grayscale: true))
        .to eq(["--page-size", "A4", "--margin-top", "20mm", "--grayscale"])
    end
  end

  describe "font (position-dependent)" do
    it "turns a single string into --font" do
      expect(argv(font: "a.ttf")).to eq(["--font", "a.ttf"])
    end

    it "puts the index immediately after, for a Hash of path and index" do
      expect(argv(font: {path: "a.ttc", index: 1}))
        .to eq(["--font", "a.ttc", "--font-index", "1"])
    end

    it "puts each font's index immediately after it, in an array" do
      # The `--font-index` is tied to "the last --font before it", so any other order
      # would apply it to a different font.
      expect(argv(font: ["a.ttf", {path: "b.ttc", index: 2}]))
        .to eq(["--font", "a.ttf", "--font", "b.ttc", "--font-index", "2"])
    end

    it "does not omit index: 0" do
      expect(argv(font: {path: "a.ttc", index: 0}))
        .to eq(["--font", "a.ttc", "--font-index", "0"])
    end

    it "accepts a Hash with string keys too" do
      expect(argv(font: {"path" => "a.ttc", "index" => 3}))
        .to eq(["--font", "a.ttc", "--font-index", "3"])
    end

    it "raises for a Hash with no path" do
      expect { argv(font: {index: 1}) }.to raise_error(ArgumentError, /needs a path/)
    end

    it "treats the other font options such as gothic_font as ordinary conversions" do
      expect(argv(gothic_font: "g.ttf", gothic_font_index: 1))
        .to eq(["--gothic-font", "g.ttf", "--gothic-font-index", "1"])
    end
  end

  describe "not validating" do
    it "puts an unknown option straight into argv (leaving the decision to clap)" do
      expect(argv(no_such_option: "x")).to eq(["--no-such-option", "x"])
    end
  end
end
