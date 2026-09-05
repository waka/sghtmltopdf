# frozen_string_literal: true

module Sghtmltopdf
  # グローバルな既定オプション。
  #
  #   Sghtmltopdf.configure do |c|
  #     c.page_size   = "A4"
  #     c.gothic_font = "/path/to/NotoSansJP-Regular.ttf"
  #   end
  #
  # ここで設定した値は`render`/`render_to_file`の引数で上書きできる
  # (マージ順はグローバル → 呼び出し時)。
  #
  # キー名の妥当性は検査しない。オプション定義はRust側(`cli/options.rs`)の
  # 1箇所に集約する方針のため、未知のキーはレンダリング時にclapが`UsageError`をraiseする。
  class Configuration
    def initialize(options = {})
      @options = {}
      # 明示的に設定した値(@options)と、Railtieなどが流し込んだ既定値
      # (@defaults)は分けて持つ。読み出しは常に@optionsが勝つので、
      # イニシャライザの実行順に依存しない。
      @defaults = {}
      options.each { |key, value| self[key] = value }
    end

    def [](key)
      key = Options.canonical_key(key)
      @options.key?(key) ? @options[key] : @defaults[key]
    end

    def []=(key, value)
      @options[Options.canonical_key(key)] = value
    end

    # @param with_defaults [Boolean] 流し込まれた既定値を含めるか。
    #   HTTPサーバへ委譲するときは`false`にする。Rails向けの既定値
    #   (`base_url`・`allow`)はローカルのファイル解決のためのもので、
    #   サーバモードではリクエストから指定できないキーだから
    def to_h(with_defaults: true)
      with_defaults ? @defaults.merge(@options) : @options.dup
    end

    # 既定値を流し込む。Railtieが Rails向けの既定値を入れるのに使う。
    # 明示的に設定された値より弱い(順序に関係なく`[]=`が勝つ)。
    def apply_defaults(defaults)
      defaults.each { |key, value| @defaults[Options.canonical_key(key)] = value }
      self
    end

    # `c.page_size = "A4"`と`c.page_size`を受ける。
    def method_missing(name, *args)
      key = name.to_s
      if key.end_with?("=")
        raise ArgumentError, "#{name}は引数1つを取ります" unless args.size == 1

        self[key.chomp("=")] = args.first
      else
        raise ArgumentError, "#{name}は引数を取りません" unless args.empty?

        self[key]
      end
    end

    def respond_to_missing?(_name, _include_private = false)
      true
    end
  end
end
