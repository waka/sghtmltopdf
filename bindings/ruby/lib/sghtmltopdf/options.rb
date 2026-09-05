# frozen_string_literal: true

require "uri"

module Sghtmltopdf
  # オプションハッシュを変換する。
  #
  # * ネイティブ拡張へ渡すCLIの引数列(argv) … [.to_argv]
  # * HTTPサーバモードへ渡すクエリ文字列   … [.to_query]
  module Options
    # 入力は常に標準入力を表す`-`を置く(実際のバイト列はFFIで直接渡すため
    # 読まれない)。出力先はRust側のSinkが決めるので、ここもダミーの`-`。
    # `-`入力のときCLIは`--output`を必須にするため、省略はできない。
    ARGV_PREFIX = ["sghtmltopdf", "-", "--output", "-"].freeze

    # Ruby側だけで解釈するキー。変換オプションではないので、argvにも
    # クエリにも出さない。
    TRANSPORT_KEYS = %i[server_url server_open_timeout server_read_timeout chunk_size].freeze

    # 別名のキー(値は正規名)。CLIは`--allow`を`--allow-path`の別名として
    # 受けるが、Ruby側は2つのキーのまま持ち回ってはいけない。既定が一方の
    # キー、呼び出し時の指定がもう一方のキーだと、ハッシュのマージでは
    # 上書きにならず両方がargvへ出てしまう(同じフラグの繰り返しは
    # 「置き換え」ではなく「合併」の意味になる)。
    ALIAS_KEYS = {allow: :allow_path}.freeze

    module_function

    # 別名のキーを正規名へ寄せる。
    def canonical_key(key)
      key = key.to_sym
      ALIAS_KEYS.fetch(key, key)
    end

    # ハッシュのキーをまとめて正規化する。
    def canonicalize(options)
      options.to_h { |key, value| [canonical_key(key), value] }
    end

    # @param options [Hash] Rubyのオプションハッシュ
    # @return [Array<String>] clapへ渡す引数列
    def to_argv(options)
      argv = ARGV_PREFIX.dup
      each_pair(options) do |name, value|
        argv.push("--#{name}")
        argv.push(value) unless value.nil?
      end
      argv
    end

    # @param options [Hash] Rubyのオプションハッシュ
    # @return [String] `POST /pdf`のクエリ文字列(先頭に`?`は付けない)
    def to_query(options)
      parts = []
      each_pair(options) do |name, value|
        # 値なしのフラグはキーだけを置く(サーバは値なし＝真として扱う)。
        parts << (value.nil? ? escape(name) : "#{escape(name)}=#{escape(value)}")
      end
      parts.join("&")
    end

    # 1つのキーと値をargvの断片へ変換する。
    #
    #   page_size: "A4"     → ["--page-size", "A4"]
    #   grayscale: true     → ["--grayscale"]
    #   grayscale: false    → []
    #   allow: ["/a", "/b"] → ["--allow", "/a", "--allow", "/b"]
    def args_for(key, value)
      pairs_for(key, value).flat_map { |name, arg| arg.nil? ? ["--#{name}"] : ["--#{name}", arg] }
    end

    # 1つのキーと値を「フラグ名と値」のペアの列にする。値が`nil`のペアは
    # 値を取らないフラグ(`--toc`など)。
    def pairs_for(key, value)
      name = flag_name(key)
      return font_pairs(value) if name == "font"

      case value
      when nil, false then []
      when true then [[name, nil]]
      # 配列は同じオプションの繰り返し。要素ごとに同じ規則を適用する。
      when Array then value.flat_map { |element| pairs_for(key, element) }
      when Hash
        # wicked_pdfの`margin: {top: 10}`のような入れ子は受けない。対応する
        # CLIフラグが無く、機械的に平坦化すると綴り違いのキーまで黙って
        # 通ってしまう。移行時は移行ガイドの対応表を見て書き換えてもらう。
        # なお単位を省いた数値の解釈はwicked_pdfと同じくmm(`cli/units.rs`)。
        example = value.keys.first
        raise ArgumentError,
          "#{key}にHashは渡せません(pathとindexを取るのは:fontだけです)。" \
          "入れ子のオプションは平坦なキーで指定してください" \
          "#{": 例 #{key}_#{example}: \"…\"" if example}"
      else [[name, value.to_s]]
      end
    end

    # `--font`と`--font-index`は出現順で対応付けられる(CLIは
    # `ArgMatches#indices_of`で「`--font-index`より手前にある最後の`--font`」
    # へ結び付ける)。そのため、フェイス番号は
    # 必ず対応する`--font`の直後へ置く。
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
        raise ArgumentError, "fontのHashにはpathが必要です: #{value.inspect}" if path.nil?

        index = value[:index] || value["index"]
        pairs = [["font", path.to_s]]
        pairs << ["font-index", index.to_s] unless index.nil?
        pairs
      else [["font", value.to_s]]
      end
    end

    # `:page_size` → `page-size`。
    def flag_name(key)
      key.to_s.tr("_", "-")
    end

    # 変換オプションだけを、渡された順にペアとして列挙する。
    def each_pair(options, &block)
      options.each do |key, value|
        key = canonical_key(key)
        next if TRANSPORT_KEYS.include?(key)

        pairs_for(key, value).each(&block)
      end
    end

    def escape(value)
      URI.encode_www_form_component(value.to_s)
    end
  end
end
