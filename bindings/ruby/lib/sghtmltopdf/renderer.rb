# frozen_string_literal: true

module Sghtmltopdf
  # Sorts the options of `render pdf: "invoice"` into three groups:
  #
  # * those passed to Rails view rendering (`render_to_string`)
  # * those passed to building the response (`send_data`)
  # * those passed to PDF conversion (`Sghtmltopdf.render`)
  #
  #
  # It is a pure Ruby class with no dependency on Rails, so it can be unit tested without Rails.
  class Renderer
    # The keys passed straight to `render_to_string`.
    RAILS_RENDER_KEYS = %i[
      action assigns body collection file formats handlers html inline layout
      locals object partial plain prefixes template variants
    ].freeze

    # The keys used to build the response (passed to `send_data`).
    RESPONSE_KEYS = %i[disposition filename status].freeze

    # The keys the renderer itself interprets (for debugging: return the HTML rather than a PDF).
    RENDERER_KEYS = %i[show_as_html].freeze

    PDF_CONTENT_TYPE = "application/pdf"
    HTML_CONTENT_TYPE = "text/html"

    attr_reader :name, :options

    # @param name [String, Symbol, nil] the value given to `pdf:` (the basis of the file name)
    # @param options [Hash] the other options given to `render`
    # @param default_name [String, nil] the file name when `name` is empty
    #   (the controller's `action_name` is expected)
    def initialize(name, options = {}, default_name: nil)
      @name = blank?(name) ? (default_name || "document").to_s : name.to_s
      @options = options.to_h { |key, value| [key.to_sym, value] }
    end

    # Register the renderer with `ActionController::Renderers.add(:pdf)`.
    # Called from the Railtie's Action Controller load hook (`on_load`).
    def self.register!
      ::ActionController::Renderers.add(:pdf) do |name, options|
        renderer = ::Sghtmltopdf::Renderer.new(name, options, default_name: action_name)
        html = render_to_string(**renderer.render_options)
        send_data(renderer.body_for(html), **renderer.send_data_options)
      end
    end

    # The options used to render the view.
    def render_options
      options.select { |key, _| RAILS_RENDER_KEYS.include?(key) }
    end

    # The options used for the PDF conversion.
    def convert_options
      known = RAILS_RENDER_KEYS + RESPONSE_KEYS + RENDERER_KEYS
      options.reject { |key, _| known.include?(key) }
    end

    # Turn the rendered HTML into the response body.
    def body_for(html)
      show_as_html? ? html : Sghtmltopdf.render(html, **convert_options)
    end

    def send_data_options
      opts = {type: content_type, disposition: disposition}
      opts[:filename] = filename unless show_as_html?
      opts[:status] = options[:status] if options.key?(:status)
      opts
    end

    def content_type
      show_as_html? ? HTML_CONTENT_TYPE : PDF_CONTENT_TYPE
    end

    # `filename: "x.pdf"` wins over `pdf: "x"`. The extension is not doubled up.
    def filename
      base = blank?(options[:filename]) ? name : options[:filename].to_s
      base.downcase.end_with?(".pdf") ? base : "#{base}.pdf"
    end

    # The default is `inline` (opened in the browser), as in wicked_pdf.
    def disposition
      blank?(options[:disposition]) ? "inline" : options[:disposition].to_s
    end

    # The equivalent of wicked_pdf's `show_as_html`. It returns the HTML rather than a PDF, so
    # the layout can be inspected in the browser's developer tools.
    def show_as_html?
      value = options[:show_as_html]
      !(value.nil? || value == false || value == "false")
    end

    private

    def blank?(value)
      value.nil? || (value.respond_to?(:empty?) && value.empty?)
    end
  end
end
