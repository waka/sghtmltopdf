# frozen_string_literal: true

# For the integration tests of `render pdf:`.
class InvoicesController < ActionController::Base
  # Render the default template (invoices/show) as a PDF.
  def show
    render pdf: "invoice"
  end

  # Passing through a file name, attachment disposition and conversion options.
  def download
    render pdf: "invoice",
      template: "invoices/show",
      filename: "請求書.pdf",
      disposition: "attachment",
      page_size: "A5"
  end

  # Specifying a layout (a key used when migrating from wicked_pdf).
  def with_layout
    render pdf: "invoice", template: "invoices/show", layout: "pdf"
  end

  # The debugging option that returns HTML rather than a PDF.
  def as_html
    render pdf: "invoice", template: "invoices/show", show_as_html: true
  end

  # The view helper inlining a CSS file from `public/` into a `<style>`.
  def with_stylesheet
    render pdf: "invoice", template: "invoices/with_stylesheet"
  end

  # `examples/receipt.html` used directly as a view. Used to check against the CLI's output
  # (the CSS stays a `<link>`, resolving `public/main.css` through `--base-url`).

  def receipt
    render pdf: "receipt", template: "invoices/receipt"
  end

  # An unknown option is rejected by clap (Sghtmltopdf::UsageError).
  def bad_option
    render pdf: "invoice", template: "invoices/show", no_such_option: "x"
  end
end
