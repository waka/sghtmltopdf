# frozen_string_literal: true

require_relative "sghtmltopdf/version"
require_relative "sghtmltopdf/options"
require_relative "sghtmltopdf/configuration"
require_relative "sghtmltopdf/renderer"

# precompiled gemはRubyのマイナーバージョンごとのディレクトリへ`.so`を置く
# (rake-compilerのクロスビルドの慣習)。開発中の`rake compile`は
# `lib/sghtmltopdf/sghtmltopdf.so`に置くので、両方を試す。
begin
  RUBY_VERSION =~ /(\d+\.\d+)/
  require "sghtmltopdf/#{Regexp.last_match(1)}/sghtmltopdf"
rescue LoadError
  begin
    require "sghtmltopdf/sghtmltopdf"
  rescue LoadError
    # どちらも無いのは、rubyプラットフォームのplaceholder gemが入っている状態
    # (経緯はgemspecの`extensions`のコメント)。対応プラットフォームなら
    # bundlerがprecompiled gemへ差し替えるので、通常ここへは来ない。
    raise LoadError, <<~MESSAGE
      sghtmltopdf could not load its native extension.

      The installed gem is the pure-ruby placeholder variant, which carries no
      renderer. It exists so that a Gemfile.lock with `PLATFORMS ruby` can
      resolve sghtmltopdf; bundler normally replaces it at install time with the
      precompiled gem for whichever platform you deploy to.

      Precompiled gems are published for:
        x86_64-linux, aarch64-linux, x86_64-linux-musl, aarch64-linux-musl,
        arm64-darwin

      This is #{RUBY_PLATFORM} on ruby #{RUBY_VERSION}.

      If your platform is on that list, bundler chose the placeholder anyway.
      That is usually `force_ruby_platform`:

          bundle config unset force_ruby_platform
          bundle install

      If it is not on the list, sghtmltopdf cannot render in this process. Run
      the renderer as a separate `sghtmltopdf server` (a container image is
      published) and call it over HTTP from here.

      https://waka.github.io/sghtmltopdf/en/usage/ruby_rails.html
    MESSAGE
  end
end

# `Error`(ネイティブ拡張が定義)を継承するため、拡張の読み込みより後に書く。
require_relative "sghtmltopdf/server_client"

module Sghtmltopdf
  # ブロック付き`render`で1回に渡すバイト数の目安(ローカル変換のみ)。
  # ページ確定ごとにブロックを呼ぶとGVLの取り直しが増えるため、ここまで
  # 溜めてから渡す。
  DEFAULT_CHUNK_SIZE = 64 * 1024

  class << self
    # HTMLを変換してPDFのバイト列(ASCII-8BITのString)を返す。
    #
    # ブロックを渡すと、PDF全体を組み立ててから返す代わりにチャンクごとに
    # ブロックを呼ぶ(返り値はnil)。Rackの`response.stream`へ流したり、S3の
    # マルチパートアップロードへ繋いだりするための口(エンジン側は出力先(sink)
    # を意識しない設計に対応する)。
    #
    #   Sghtmltopdf.render(html) { |bytes| response.stream.write(bytes) }
    #
    # ローカル・サーバ委譲のどちらでも、PDF全体が組み上がるのを待たずに
    # 書き出せる(ローカルは確定したページから順に、サーバは`?stream=1`の
    # chunked transfer encodingをそのまま渡す)。
    #
    # ただし逐次になるのはPDFの書き出しだけで、HTMLのパースとレイアウトは
    # 文書全体に対して先に行う。最初のチャンクが届くのは変換の終盤で、
    # ピークメモリもブロック無しの場合と変わらない。HTMLを読みながら
    # ページを確定させたい場合は`streaming: true`と併せて使う
    # (制約と引き換えにメモリが大きく減る)。
    #
    # 1回に渡すバイト数の目安は`chunk_size:`で変えられる(既定64KiB。
    # ローカル変換のみ。小さくするとGVLの取り直しが増える)。
    def render(html, **options, &block)
      client = server_client(options)
      return client.render(html.to_s, server_options(options), &block) if client
      return Native.render(html.to_s, argv_for(options)) if block.nil?

      Native.render_each(html.to_s, argv_for(options), block, chunk_size(options))
      nil
    end

    # HTMLを変換して`path`へ書き出す。
    #
    # 一時ファイルへ書いて成功時だけrenameするため、途中で失敗しても
    # 壊れたPDFが出力先に残らない(サーバへ委譲する場合も同じ)。
    def render_to_file(html, path, **options)
      client = server_client(options)
      return client.render_to_file(html.to_s, server_options(options), path.to_s) if client

      Native.render_to_file(html.to_s, argv_for(options), path.to_s)
      nil
    end

    # グローバルな既定オプション。
    def configure
      yield config
      config
    end

    def config
      @config ||= Configuration.new
    end

    # 主にテスト用。設定を空に戻す。
    def reset_config!
      @config = Configuration.new
    end

    private

    # グローバル設定 → 呼び出し時オプションの順にマージしてargvにする。
    def argv_for(options)
      Options.to_argv(config.to_h.merge(options))
    end

    # `server_url`があればサーバへ委譲する。タイムアウトも同じ順でマージする。
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

    # ブロックへ1回に渡すバイト数の目安(ローカル変換のみ)。
    def chunk_size(options)
      value = config.to_h.merge(options)[:chunk_size]
      value.nil? ? DEFAULT_CHUNK_SIZE : Integer(value)
    end

    # サーバへ渡すオプション。流し込まれた既定値は外す
    # (Rails向けの`base_url`・`allow`はローカルのファイル解決のための
    # 既定値で、サーバモードではリクエストから指定できず400になる。
    # 明示的に設定した値はそのまま送り、可否はサーバに判断させる)。
    def server_options(options)
      config.to_h(with_defaults: false).merge(options)
    end
  end
end

require_relative "sghtmltopdf/railtie" if defined?(::Rails::Railtie)
