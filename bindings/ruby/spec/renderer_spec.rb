# frozen_string_literal: true

# Unit tests for Renderer alone. They check only the sorting, without loading Rails.
RSpec.describe Sghtmltopdf::Renderer do
  def renderer(name = "invoice", **options)
    described_class.new(name, options)
  end

  describe "sorting the options" do
    subject(:pdf) { renderer(template: "invoices/show", layout: "pdf", page_size: "A4", margin_top: "20mm") }

    it "passes the Rails keys to view rendering" do
      expect(pdf.render_options).to eq(template: "invoices/show", layout: "pdf")
    end

    it "makes everything else a conversion option" do
      expect(pdf.convert_options).to eq(page_size: "A4", margin_top: "20mm")
    end

    it "keeps the response keys out of the conversion options" do
      pdf = renderer(disposition: "attachment", filename: "x.pdf", status: 201, show_as_html: false)
      expect(pdf.convert_options).to be_empty
      expect(pdf.render_options).to be_empty
    end

    it "accepts string keys too" do
      pdf = described_class.new("invoice", {"template" => "a/b", "page_size" => "A4"})
      expect(pdf.render_options).to eq(template: "a/b")
      expect(pdf.convert_options).to eq(page_size: "A4")
    end
  end

  describe "#filename" do
    it "adds the extension to the value of pdf:" do
      expect(renderer("invoice").filename).to eq("invoice.pdf")
    end

    it "does not double up the extension" do
      expect(renderer("invoice.pdf").filename).to eq("invoice.pdf")
      expect(renderer("invoice.PDF").filename).to eq("invoice.PDF")
    end

    it "lets filename: win when present" do
      expect(renderer("invoice", filename: "請求書").filename).to eq("請求書.pdf")
    end

    it "uses default_name when pdf: is empty" do
      expect(described_class.new(nil, {}, default_name: "show").filename).to eq("show.pdf")
      expect(described_class.new("", {}, default_name: "show").filename).to eq("show.pdf")
    end

    it "falls back to document with no default_name either" do
      expect(described_class.new(nil, {}).filename).to eq("document.pdf")
    end
  end

  describe "#disposition" do
    it "defaults to inline" do
      expect(renderer.disposition).to eq("inline")
    end

    it "uses what was given" do
      expect(renderer(disposition: :attachment).disposition).to eq("attachment")
    end
  end

  describe "#send_data_options" do
    it "returns the PDF Content-Type and the file name" do
      expect(renderer.send_data_options)
        .to eq(type: "application/pdf", disposition: "inline", filename: "invoice.pdf")
    end

    it "passes status: through" do
      expect(renderer(status: 201).send_data_options[:status]).to eq(201)
    end

    it "returns HTML with show_as_html (and no file name)" do
      expect(renderer(show_as_html: true).send_data_options)
        .to eq(type: "text/html", disposition: "inline")
    end
  end

  describe "#body_for" do
    let(:html) { "<h1>Invoice</h1>" }

    it "converts to PDF by default" do
      expect(renderer.body_for(html)).to start_with("%PDF-")
    end

    it "honours the conversion options" do
      a4 = normalize(renderer(page_size: "A4").body_for(html))
      a5 = normalize(renderer(page_size: "A5").body_for(html))
      expect(a4).not_to eq(a5)
    end

    it "returns the HTML unchanged with show_as_html" do
      expect(renderer(show_as_html: true).body_for(html)).to eq(html)
    end
  end
end
