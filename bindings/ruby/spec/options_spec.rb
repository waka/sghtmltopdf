# frozen_string_literal: true

RSpec.describe Sghtmltopdf::Options do
  # The first entries of argv are always fixed, so they are dropped to make comparison easier.
  def argv(options)
    described_class.to_argv(options).drop(Sghtmltopdf::Options::ARGV_PREFIX.size)
  end

  it "always prefixes the standard streams for input and output" do
    expect(described_class.to_argv({})).to eq(["sghtmltopdf", "-", "--output", "-"])
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

    it "turns an array into a repeated option" do
      expect(argv(allow: ["/a", "/b"])).to eq(["--allow", "/a", "--allow", "/b"])
    end

    it "applies the same rules to true/false inside an array" do
      expect(argv(allow: ["/a", nil, "/b"])).to eq(["--allow", "/a", "--allow", "/b"])
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
