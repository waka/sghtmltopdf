# frozen_string_literal: true

# Stream to the Rack response page by page as they settle.
#
# The `render pdf:` renderer returns the assembled PDF in one go with `send_data`, so to
# return it incrementally you combine `ActionController::Live` with a block-taking `render`
# directly.
class StreamsController < ActionController::Base
  include ActionController::Live

  def show
    response.headers["Content-Type"] = "application/pdf"
    html = render_to_string(template: "invoices/long", layout: false)
    Sghtmltopdf.render(html, chunk_size: 1024) { |bytes| response.stream.write(bytes) }
  ensure
    response.stream.close
  end
end
