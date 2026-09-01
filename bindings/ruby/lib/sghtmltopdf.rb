# frozen_string_literal: true

require_relative "sghtmltopdf/version"
require_relative "sghtmltopdf/options"
require_relative "sghtmltopdf/configuration"
require_relative "sghtmltopdf/renderer"

# A precompiled gem puts the `.so` in a directory per Ruby minor version
# (rake-compiler's cross-build convention). During development `rake compile` puts it in
# `lib/sghtmltopdf/sghtmltopdf.so`, so both are tried.
begin
  RUBY_VERSION =~ /(\d+\.\d+)/
  require "sghtmltopdf/#{Regexp.last_match(1)}/sghtmltopdf"
rescue LoadError
  require "sghtmltopdf/sghtmltopdf"
end

# It inherits `Error` (defined by the native extension), so it comes after the extension is loaded.
require_relative "sghtmltopdf/server_client"

module Sghtmltopdf
  # The rough number of bytes handed over per call to a block-taking `render` (local conversion only).
  # Calling the block on every settled page would mean reacquiring the GVL too often, so this
  # much is accumulated first.
  DEFAULT_CHUNK_SIZE = 64 * 1024

  class << self
    # Convert HTML and return the PDF bytes (an ASCII-8BIT String).
    #
    # Given a block, it calls the block per chunk rather than assembling the whole PDF first
    # (returning nil). It is the hook for streaming into Rack's `response.stream` or into an
    # S3 multipart upload (matching the engine's design, which is unaware of the output sink).
    #
    #   Sghtmltopdf.render(html) { |bytes| response.stream.write(bytes) }
    #
    # Both locally and when delegating to a server, it can be written out without waiting for
    # the whole PDF to be assembled (locally page by page as they settle; from a server, the
    # `?stream=1` chunked transfer encoding passed straight through).
    #
    # Only the PDF writing is incremental, though: HTML parsing and layout still happen for
    # the whole document first. The first chunk arrives late in the conversion, and the peak
    # memory is no different from the block-less case. To settle pages while the HTML is being
    # read, use it together with `streaming: true`
    # (which trades constraints for a large reduction in memory).
    #
    # The rough bytes per call can be changed with `chunk_size:` (64KiB by default; local
    # conversion only; a smaller value means reacquiring the GVL more often).
    def render(html, **options, &block)
      client = server_client(options)
      return client.render(html.to_s, server_options(options), &block) if client
      return Native.render(html.to_s, argv_for(options)) if block.nil?

      Native.render_each(html.to_s, argv_for(options), block, chunk_size(options))
      nil
    end

    # Convert HTML and write it to `path`.
    #
    # It writes to a temporary file and renames only on success, so a failure part-way through
    # leaves no broken PDF at the destination (the same when delegating to a server).
    def render_to_file(html, path, **options)
      client = server_client(options)
      return client.render_to_file(html.to_s, server_options(options), path.to_s) if client

      Native.render_to_file(html.to_s, argv_for(options), path.to_s)
      nil
    end

    # The global default options.
    def configure
      yield config
      config
    end

    def config
      @config ||= Configuration.new
    end

    # Mainly for tests. Resets the configuration to empty.
    def reset_config!
      @config = Configuration.new
    end

    private

    # Merge in the order global settings, then call-time options, and turn that into argv.
    def argv_for(options)
      Options.to_argv(config.to_h.merge(options))
    end

    # With a `server_url` it delegates to the server. The timeout merges in the same order.
    def server_client(options)
      merged = config.to_h.merge(options)
      url = merged[:server_url]
      return nil if url.nil? || url.to_s.empty?

      ServerClient.new(
        url,
        open_timeout: merged[:server_open_timeout],
        read_timeout: merged[:server_read_timeout]
      )
    end

    # The rough bytes handed to the block per call (local conversion only).
    def chunk_size(options)
      value = config.to_h.merge(options)[:chunk_size]
      value.nil? ? DEFAULT_CHUNK_SIZE : Integer(value)
    end

    # The options passed to the server. The injected defaults are removed
    # (the Rails-oriented `base_url` and `allow` are defaults for local file resolution, and
    # server mode cannot take them from a request, giving a 400.
    # An explicitly set value is sent as-is and the server decides whether to accept it).
    def server_options(options)
      config.to_h(with_defaults: false).merge(options)
    end
  end
end

require_relative "sghtmltopdf/railtie" if defined?(::Rails::Railtie)
