# frozen_string_literal: true

require "uri"

module Sghtmltopdf
  # Converts an options hash into:
  #
  # * the CLI argument list (argv) passed to the native extension ... [.to_argv]
  # * the query string passed to HTTP server mode                 ... [.to_query]
  module Options
    # The input is always `-`, meaning standard input (the real bytes are passed directly over
    # FFI and never read). The destination is decided by the Rust-side Sink, so that is a
    # dummy `-` too. With a `-` input the CLI requires `--output`, so it cannot be omitted.
    ARGV_PREFIX = ["sghtmltopdf", "-", "--output", "-"].freeze

    # The keys interpreted on the Ruby side alone. They are not conversion options, so they
    # appear in neither the argv nor the query.
    TRANSPORT_KEYS = %i[server_url server_open_timeout server_read_timeout chunk_size].freeze

    module_function

    # @param options [Hash] the Ruby options hash
    # @return [Array<String>] the argument list passed to clap
    def to_argv(options)
      argv = ARGV_PREFIX.dup
      each_pair(options) do |name, value|
        argv.push("--#{name}")
        argv.push(value) unless value.nil?
      end
      argv
    end

    # @param options [Hash] the Ruby options hash
    # @return [String] the query string for `POST /pdf` (with no leading `?`)
    def to_query(options)
      parts = []
      each_pair(options) do |name, value|
        # A valueless flag becomes just the key (the server treats no value as true).
        parts << (value.nil? ? escape(name) : "#{escape(name)}=#{escape(value)}")
      end
      parts.join("&")
    end

    # Convert one key and value into an argv fragment.
    #
    #   page_size: "A4"     → ["--page-size", "A4"]
    #   grayscale: true     → ["--grayscale"]
    #   grayscale: false    → []
    #   allow: ["/a", "/b"] → ["--allow", "/a", "--allow", "/b"]
    def args_for(key, value)
      pairs_for(key, value).flat_map { |name, arg| arg.nil? ? ["--#{name}"] : ["--#{name}", arg] }
    end

    # Turn one key and value into a list of "flag name and value" pairs. A pair whose value is
    # `nil` is a flag taking no value (`--toc` and the like).
    def pairs_for(key, value)
      name = flag_name(key)
      return font_pairs(value) if name == "font"

      case value
      when nil, false then []
      when true then [[name, nil]]
      # An array means the same option repeated. The same rule applies to each element.
      when Array then value.flat_map { |element| pairs_for(key, element) }
      when Hash
        # Nesting such as wicked_pdf's `margin: {top: 10}` is not accepted. There is no
        # corresponding CLI flag, and flattening it mechanically would let even misspelled
        # keys through silently. When migrating, use the correspondence table in the
        # migration guide to rewrite them. A unitless number is read as mm, as in wicked_pdf (`cli/units.rs`).
        example = value.keys.first
        raise ArgumentError,
          "a Hash cannot be passed for #{key} (only :font takes a path and an index). " \
          "Give nested options as flat keys" \
          "#{": for example #{key}_#{example}: \"...\"" if example}"
      else [[name, value.to_s]]
      end
    end

    # `--font` and `--font-index` are paired by their order of appearance (the CLI ties each
    # `--font-index` to "the last `--font` before it" via `ArgMatches#indices_of`).
    # So a face index always goes immediately after its own `--font`.
    #
    #   font: "a.ttf"                        → ["--font", "a.ttf"]
    #   font: {path: "a.ttc", index: 1}      → ["--font", "a.ttc", "--font-index", "1"]
    #   font: ["a.ttf", {path: "b.ttc", index: 2}]
    #     → ["--font", "a.ttf", "--font", "b.ttc", "--font-index", "2"]
    def font_args(value)
      font_pairs(value).flat_map { |name, arg| ["--#{name}", arg] }
    end

    def font_pairs(value)
      case value
      when nil, false then []
      when Array then value.flat_map { |element| font_pairs(element) }
      when Hash
        path = value[:path] || value["path"]
        raise ArgumentError, "a font Hash needs a path: #{value.inspect}" if path.nil?

        index = value[:index] || value["index"]
        pairs = [["font", path.to_s]]
        pairs << ["font-index", index.to_s] unless index.nil?
        pairs
      else [["font", value.to_s]]
      end
    end

    # `:page_size` becomes `page-size`.
    def flag_name(key)
      key.to_s.tr("_", "-")
    end

    # Enumerate only the conversion options, as pairs, in the order they were given.
    def each_pair(options, &block)
      options.each do |key, value|
        next if TRANSPORT_KEYS.include?(key.to_sym)

        pairs_for(key, value).each(&block)
      end
    end

    def escape(value)
      URI.encode_www_form_component(value.to_s)
    end
  end
end
