# frozen_string_literal: true

require "action_controller"
require "sghtmltopdf/renderer"

RSpec.describe "PDF controller rendering" do
  let(:controller) do
    Class.new(ActionController::Base).new.tap do |instance|
      instance.set_request!(ActionDispatch::TestRequest.create)
      instance.set_response!(ActionDispatch::TestResponse.new)
    end
  end

  before do
    Sghtmltopdf::Renderer.register!
    allow(Sghtmltopdf).to receive(:render).with("Invoice", page_size: "A5").and_return("%PDF-test")
  end

  it "returns PDF bytes without sending a response, then allows send_data" do
    options = {pdf: "invoice", inline: "Invoice", layout: false,
               page_size: "A5", filename: "unused.pdf", status: 201}
    headers = controller.response.headers.to_h.dup

    expect(controller.render_to_string(options)).to eq("%PDF-test")
    expect(options[:pdf]).to eq("invoice")
    expect(controller.response_body).to be_nil
    expect(controller.performed?).to be_falsey
    expect(controller.status).to eq(200)
    expect(controller.response.headers.to_h).to eq(headers)

    controller.send(:send_data, "%PDF-test", type: "application/pdf", filename: "invoice.pdf")
    expect(controller.response_body.join).to eq("%PDF-test")
  end

  it "accepts PDF options in the second argument without sending a response" do
    options = {pdf: "invoice", inline: "Invoice", layout: false,
               page_size: "A5", filename: "unused.pdf", status: 201}.freeze
    headers = controller.response.headers.to_h.dup

    expect(controller.render_to_string(nil, options)).to eq("%PDF-test")
    expect(controller.response_body).to be_nil
    expect(controller.performed?).to be_falsey
    expect(controller.status).to eq(200)
    expect(controller.response.headers.to_h).to eq(headers)

    controller.send(:send_data, "%PDF-test", type: "application/pdf", filename: "invoice.pdf")
    expect(controller.response_body.join).to eq("%PDF-test")
  end

  it "still sends a PDF response from render" do
    controller.render(pdf: "invoice", inline: "Invoice", page_size: "A5", status: 201)

    expect(controller.response_body.join).to eq("%PDF-test")
    expect(controller.status).to eq(201)
    expect(controller.media_type).to eq("application/pdf")
    expect(controller.headers["Content-Disposition"]).to include('filename="invoice.pdf"')
  end

  it "returns HTML for show_as_html without sending a response" do
    expect(controller.render_to_string(pdf: "invoice", inline: "Invoice", show_as_html: true)).to eq("Invoice")
    expect(controller.response_body).to be_nil
    expect(Sghtmltopdf).not_to have_received(:render)
  end

  it "delegates ordinary render_to_string to Rails" do
    expect(controller.render_to_string(inline: "Invoice")).to eq("Invoice")
    expect(controller.response_body).to be_nil
    expect(Sghtmltopdf).not_to have_received(:render)
  end
end
