# frozen_string_literal: true

module Sghtmltopdf
  # The global default options.
  #
  #   Sghtmltopdf.configure do |c|
  #     c.page_size   = "A4"
  #     c.gothic_font = "/path/to/NotoSansJP-Regular.ttf"
  #   end
  #
  # A value set here can be overridden by an argument to `render`/`render_to_file`
  # (merged in the order global, then call-time).
  #
  # Key names are not validated. The option definitions live in one place on the Rust side
  # (`cli/options.rs`), so an unknown key makes clap raise a `UsageError` at render time.
  class Configuration
    def initialize(options = {})
      @options = {}
      # Values set explicitly (@options) are kept separately from the defaults injected by
      # the Railtie and others (@defaults). Reads always prefer @options, so nothing depends
      # on the order the initialisers run in.
      @defaults = {}
      options.each { |key, value| self[key] = value }
    end

    def [](key)
      key = key.to_sym
      @options.key?(key) ? @options[key] : @defaults[key]
    end

    def []=(key, value)
      @options[key.to_sym] = value
    end

    # @param with_defaults [Boolean] whether to include the injected defaults.
    #   Set it to `false` when delegating to the HTTP server: the Rails-oriented defaults
    #   (`base_url` and `allow`) exist for local file resolution and are keys server mode
    #   cannot take from a request
    def to_h(with_defaults: true)
      with_defaults ? @defaults.merge(@options) : @options.dup
    end

    # Inject the defaults. Used by the Railtie to set the Rails-oriented defaults.
    # They are weaker than explicitly set values (`[]=` wins regardless of order).
    def apply_defaults(defaults)
      defaults.each { |key, value| @defaults[key.to_sym] = value }
      self
    end

    # Accepts both `c.page_size = "A4"` and `c.page_size`.
    def method_missing(name, *args)
      key = name.to_s
      if key.end_with?("=")
        raise ArgumentError, "#{name} takes one argument" unless args.size == 1

        self[key.chomp("=")] = args.first
      else
        raise ArgumentError, "#{name} takes no arguments" unless args.empty?

        self[key]
      end
    end

    def respond_to_missing?(_name, _include_private = false)
      true
    end
  end
end
