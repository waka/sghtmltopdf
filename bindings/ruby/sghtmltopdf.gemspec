# frozen_string_literal: true

require_relative "lib/sghtmltopdf/version"

Gem::Specification.new do |spec|
  spec.name = "sghtmltopdf"
  spec.version = Sghtmltopdf::VERSION
  spec.authors = ["yo_waka"]
  spec.email = ["y.wakahara@gmail.com"]

  spec.summary = "HTML to PDF renderer that does not depend on Chromium, WebKit, or Gecko"
  spec.description = <<~DESC
    Ruby binding for sghtmltopdf, a successor to wkhtmltopdf: an HTML-to-PDF
    rendering engine written in Rust that needs no browser process. The engine
    runs in-process through a native extension and releases the GVL while
    rendering. On Rails it registers a wicked_pdf compatible renderer, so
    `render pdf: "invoice"` works with the keys you already use.
  DESC
  spec.homepage = "https://github.com/waka/sghtmltopdf"
  spec.license = "MIT"
  spec.required_ruby_version = ">= 3.2.0"

  spec.metadata["source_code_uri"] = spec.homepage
  spec.metadata["changelog_uri"] = "#{spec.homepage}/blob/main/CHANGELOG.md"
  spec.metadata["bug_tracker_uri"] = "#{spec.homepage}/issues"
  spec.metadata["rubygems_mfa_required"] = "true"

  # `gem build`はgemspecのあるディレクトリ配下しか集められないため、
  # LICENSEはリポジトリルートからコピーしたものを置いてある。
  #
  # ext/やCargo.*は入れない。rubyプラットフォームのgemはビルドしないので使わず、
  # precompiled gemからはrb_sysがどうせ剥がすため(後述)。
  spec.files = Dir[
    "lib/**/*.rb",
    "LICENSE*",
    "README*"
  ]
  spec.require_paths = ["lib"]

  # `extensions`は意図的に宣言しない。
  #
  # このgemは対応プラットフォーム向けのprecompiled gemとして配布していて、
  # rubyプラットフォームのgemは「ビルドを試みないplaceholder」にしてある。
  # 狙いは`PLATFORMS`が`ruby`だけのGemfile.lockをそのまま使えるようにすること:
  # bundlerはlockfileの`ruby`をインストール時にローカルのプラットフォームへ
  # 解決し直すので、利用側は`bundle lock --add-platform`をしなくても
  # デプロイ先ごとに適切なprecompiled gemが入る(lockfileも書き換わらない)。
  #
  # ここで`extensions`を宣言してしまうと、precompiled gemが無い環境
  # (Intel Mac・Windowsなど)へフォールバックしたときにビルドが走る。
  # ソースgemにRustコアを同梱していないので成功し得ず、`bundle install`ごと
  # 落ちてしまう。読み込み時の案内は`lib/sghtmltopdf.rb`が出す。
  #
  # precompiled gemのビルドには影響しない。rake-compilerは`extensions`ではなく
  # ext/のCargo.tomlを見てコンパイルし、native gemでは`extensions`を空にする。
end
