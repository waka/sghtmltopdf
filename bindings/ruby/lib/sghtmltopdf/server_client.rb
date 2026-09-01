# frozen_string_literal: true

require "net/http"
require "uri"

module Sghtmltopdf
  class ServerError < Error; end

  # The client delegating conversion to HTTP server mode (`sghtmltopdf server`).
  #
  #   Sghtmltopdf.configure { |c| c.server_url = "http://pdf.internal:8080" }
  #   pdf = Sghtmltopdf.render(html, page_size: "A4")
  class ServerClient
    DEFAULT_OPEN_TIMEOUT = 5
    DEFAULT_READ_TIMEOUT = 120

    # The rough size of each read. With `?stream=1` this is the unit handed to the block.
    CHUNK_SIZE = 64 * 1024

    attr_reader :uri, :open_timeout, :read_timeout

    # @param url [String] the server's base URL (`http://host:port`)
    def initialize(url, open_timeout: nil, read_timeout: nil)
      @uri = parse(url)
      @open_timeout = (open_timeout || DEFAULT_OPEN_TIMEOUT).to_f
      @read_timeout = (read_timeout || DEFAULT_READ_TIMEOUT).to_f
    end

    # Convert HTML to PDF.
    #
    # Given a block it uses `?stream=1` (chunked transfer encoding) and hands over chunks as
    # the server settles each page. With no block it returns the whole PDF as a String.
    def render(html, options, &block)
      request = build_request(html, options, stream: !block.nil?)
      pdf = nil
      start do |http|
        # With a block, `request` returns the response object, so the result is received in an
        # outer variable.
        http.request(request) do |response|
          ensure_success!(response)
          if block
            response.read_body { |chunk| block.call(chunk.b) }
          else
            pdf = read_all(response)
          end
        end
      end
      pdf
    end

    # Write the conversion result to `path`. To leave no broken PDF on a failure part-way
    # through, it writes to a temporary file and renames (matching the behaviour of the
    # native extension's `FileSink`).
    def render_to_file(html, options, path)
      tmp = "#{path}.#{Process.pid}.tmp"
      begin
        File.open(tmp, "wb") do |file|
          render(html, options) { |chunk| file.write(chunk) }
        end
      rescue SystemCallError => e
        File.unlink(tmp) if File.exist?(tmp)
        raise InputError, "failed to write to #{path}: #{e.message}"
      rescue StandardError
        File.unlink(tmp) if File.exist?(tmp)
        raise
      end
      File.rename(tmp, path)
      nil
    end

    private

    def parse(url)
      uri = URI.parse(url.to_s)
      unless uri.is_a?(URI::HTTP) && uri.host
        raise ArgumentError, "server_url must be an http(s) URL: #{url.inspect}"
      end

      uri
    end

    def build_request(html, options, stream:)
      query = Options.to_query(options)
      query = stream ? [query, "stream=1"].reject(&:empty?).join("&") : query
      target = uri.dup
      target.path = "/pdf"
      target.query = query.empty? ? nil : query

      request = Net::HTTP::Post.new(target)
      request["Content-Type"] = "text/html; charset=utf-8"
      request.body = html.to_s.b
      request
    end

    def start(&block)
      Net::HTTP.start(
        uri.host, uri.port,
        use_ssl: uri.scheme == "https",
        open_timeout: open_timeout,
        read_timeout: read_timeout,
        &block
      )
    rescue Net::OpenTimeout, Net::ReadTimeout => e
      raise ServerError, "the connection to #{base} timed out: #{e.class}"
    rescue SocketError, SystemCallError, IOError, OpenSSL::SSL::SSLError => e
      raise ServerError, "the connection to #{base} failed: #{e.message}"
    end

    # An error response's body is a `text/plain` message (the same wording as the CLI).
    def ensure_success!(response)
      return if response.is_a?(Net::HTTPOK)

      message = read_all(response).force_encoding(Encoding::UTF_8).strip
      raise error_class(response), "#{base}: #{message}"
    end

    def error_class(response)
      case response.code.to_i
      when 400 then UsageError
      when 413 then InputError
      when 500 then RenderError
      # A 404 or 405 means a wrong path or method, most likely because the other end is not
      # an sghtmltopdf server. A 503 or 504 means the queue overflowed or the queue wait timed out.
      else ServerError
      end
    end

    def read_all(response)
      buffer = +""
      response.read_body { |chunk| buffer << chunk }
      buffer.b
    end

    def base
      "#{uri.scheme}://#{uri.host}:#{uri.port}"
    end
  end
end
