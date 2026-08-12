# frozen_string_literal: true

# このextconfが動くのはprecompiled gemを作るとき(`rake compile` /
# `rake native:<platform> gem` / CIのcross-gem)だけ。配布するgemのうち
# rubyプラットフォームのものは`extensions`を宣言しておらず、利用者の環境で
# ここが動くことはない(gemspecの`extensions`のコメント参照)。
require "mkmf"

# Rustコアはリポジトリの`core/`をpath依存で参照している。`rb_sys/mkmf`の
# requireより先に確かめる。順番を逆にすると、コアが無いときに
# `cannot load such file -- rb_sys/mkmf`だけが出て理由が分からなくなる
# (rb_sysは開発用の依存で、gemのインストール時には無いことがある)。
core = File.expand_path("../../../../core", __dir__)
unless File.exist?(File.join(core, "Cargo.toml"))
  abort <<~MESSAGE
    sghtmltopdf: Rustコア(#{core})が見つかりません。

    この拡張はリポジトリ全体のチェックアウトからビルドする前提です
    (extクレートは`core/`をpath依存で参照しています)。
    precompiled gemを作るときは`bindings/ruby`で
    `rake native:<platform> gem`(CIではoxidize-rb/actions/cross-gem)を
    使ってください。
  MESSAGE
end

require "rb_sys/mkmf"

# `lib/sghtmltopdf/sghtmltopdf.so`として作る。
create_rust_makefile("sghtmltopdf/sghtmltopdf")
