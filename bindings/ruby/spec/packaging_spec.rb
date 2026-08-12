# frozen_string_literal: true

# gemの「形」に対する回帰テスト。
#
# rubyプラットフォームのgemはビルドを試みないplaceholderでなければならない。
# ここが崩れると、precompiled gemが無い環境へフォールバックしたときに
# ビルドが走って`bundle install`ごと落ちる(経緯はgemspecのコメント)。
RSpec.describe "gemのパッケージング" do
  def gemspec
    dir = File.expand_path("..", __dir__)
    path = File.join(dir, "sghtmltopdf.gemspec")
    skip "gemspecが無い(gemとして配布された状態では見えない)" unless File.exist?(path)

    # `spec.files`の`Dir[]`はカレントディレクトリ基準なので合わせる。
    @gemspec ||= Dir.chdir(dir) { Gem::Specification.load(path) }
  end

  it "拡張を宣言していない(rubyプラットフォームのgemがビルドを試みない)" do
    expect(gemspec.extensions).to be_empty
  end

  it "Rustのソースやビルド定義を同梱していない" do
    offenders = gemspec.files.grep(/\.rs\z|Cargo\.(toml|lock)\z|extconf\.rb\z/)

    expect(offenders).to be_empty
  end

  it "ライブラリ本体を同梱している" do
    expect(gemspec.files).to include("lib/sghtmltopdf.rb", "lib/sghtmltopdf/version.rb")
  end

  it "ネイティブ拡張のビルド結果を同梱していない(precompiled gem側で足される)" do
    expect(gemspec.files.grep(/\.(so|bundle|dylib|dll)\z/)).to be_empty
  end
end
