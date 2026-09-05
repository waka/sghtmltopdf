# frozen_string_literal: true

require "open3"

# アセットパイプライン(Propshaft)を入れたアプリでの経路。
#
# dummyアプリ本体はパイプラインgem無しで動かしている(素のRailsアプリの
# 既定を見るため)ので、パイプラインを読み込んだアプリは別プロセスで起動する。
# `spec/railtie_spec.rb`と同じやり方。
#
# ここで見たいのは開発環境の状況、つまり「precompileしていないので
# `public/assets`には何も無く、実ファイルは`app/assets`にある」状態。
RSpec.describe "アセットパイプラインを入れたアプリ" do
  PIPELINE_ROOT = File.expand_path("..", __dir__)

  # 子プロセスでPropshaft入りのdummyアプリを起動し、`script`を評価する。
  def in_pipeline_app(script)
    boot = <<~RUBY
      ENV["RAILS_ENV"] = "test"
      require "logger"
      require "rails"
      require "action_controller/railtie"
      require "propshaft"
      require "sghtmltopdf"
      require "sghtmltopdf/railtie"

      module PipelineDummy
        class Application < ::Rails::Application
          config.load_defaults("\#{::Rails::VERSION::MAJOR}.\#{::Rails::VERSION::MINOR}")
          config.root = #{File.join(PIPELINE_ROOT, "spec/dummy").inspect}
          config.eager_load = false
          config.secret_key_base = "sghtmltopdf" * 8
          config.logger = Logger.new(IO::NULL)
          config.hosts.clear
        end
      end
      Rails.application.initialize!

      view = ActionController::Base.helpers
      root = Rails.root.to_s
    RUBY

    out, err, status = Open3.capture3(
      RbConfig.ruby, "-I#{File.join(PIPELINE_ROOT, "lib")}", "-rbundler/setup",
      "-e", boot + script, chdir: PIPELINE_ROOT
    )
    raise "子プロセスが失敗しました: #{err}" unless status.success?

    out.split("\n")
  end

  it "allow_pathの既定にパイプラインのロードパスが入る" do
    lines = in_pipeline_app(<<~RUBY)
      allow = Sghtmltopdf.config[:allow_path]
      puts allow.include?(File.join(root, "public"))
      puts allow.include?(File.join(root, "app/assets/images"))
      # gemが提供するアセットのパスも入る(Rails.rootの外)。
      puts allow.any? { |dir| !dir.start_with?(root) }
      # config/はもう読める範囲に入らない。
      puts allow.none? { |dir| dir == root }
    RUBY

    expect(lines).to eq(%w[true true true true])
  end

  # Propshaftは自分のロードパスに無いアセットを渡されるとMissingAssetErrorを
  # 投げる。`public/`にだけあるファイルがまさにそれなので、素通しすると
  # `from_public_dir`が例外で落ちる。
  it "public/にだけあるファイルもパイプライン越しに解決できる" do
    lines = in_pipeline_app(<<~RUBY)
      puts view.sghtmltopdf_asset_path("logo.png") == File.join(root, "public/logo.png")
      puts view.sghtmltopdf_asset_path("pipeline-logo.png") ==
        File.join(root, "app/assets/images/pipeline-logo.png")
      puts view.sghtmltopdf_asset_path("no-such-file.png").nil?
    RUBY

    expect(lines).to eq(%w[true true true])
  end

  # 素の`image_tag`はダイジェスト付きの仮想パスを出すだけで、devでは
  # 対応する実ファイルがどこにも無い。ヘルパはロードパスを引いて実体を指す。
  it "素のimage_tagが出す仮想パスには実ファイルが無い" do
    lines = in_pipeline_app(<<~RUBY)
      src = view.image_tag("pipeline-logo.png")[/src="([^"]+)"/, 1]
      puts src.start_with?("/assets/pipeline-logo-")
      puts File.file?(File.join(root, "public", src))
      puts File.file?(src)
    RUBY

    expect(lines).to eq(%w[true false false])
  end

  it "public/の外の画像は絶対パスで指し、エンジンが読める" do
    lines = in_pipeline_app(<<~RUBY)
      html = view.sghtmltopdf_image_tag("pipeline-logo.png")
      src = html[/src="([^"]+)"/, 1]
      puts src == File.join(root, "app/assets/images/pipeline-logo.png")
      puts html.include?("data:")
      # 20x16のPNGがXObjectとして埋まる。
      puts Sghtmltopdf.render(html).include?("/Width 20")
    RUBY

    expect(lines).to eq(%w[true false true])
  end

  # 開発環境ではヘルパがコンパイル前のCSSを引くので、`url()`はパイプラインに
  # 書き換えられておらず論理パスのまま残る。エンジンは文書のbase_url基準でしか
  # 解決できないので、ここでロードパスを引いて実体へ指し直す。
  it "パイプラインのCSSのurl()はロードパス越しに実体を指す" do
    lines = in_pipeline_app(<<~'RUBY')
      html = view.sghtmltopdf_stylesheet_link_tag("pipeline")
      src = html[/url\("([^"]+)"\)/, 1]
      puts src == File.join(root, "app/assets/images/pipeline-logo.png")
      puts html.include?("data:")
      # 20x16のPNGがXObjectとして埋まる。
      puts Sghtmltopdf.render(html + "<p>x</p>").include?("/Width 20")
    RUBY

    expect(lines).to eq(%w[true false true])
  end

  # devでは`public/assets`に何も無いので、`/assets/…`はロードパスの論理パスへ
  # 読み替える。マウント位置(`config.assets.prefix`)は論理パスの一部ではない。
  it "/assets/の参照はマウント位置を外してロードパスから引く" do
    lines = in_pipeline_app(<<~'RUBY')
      require "fileutils"
      css = File.join(root, "public/rooted.css")
      begin
        File.write(css, %(body { background-image: url("/assets/pipeline-logo.png"); }))
        html = view.sghtmltopdf_stylesheet_link_tag("rooted")
        puts html.include?(File.join(root, "app/assets/images/pipeline-logo.png"))
      ensure
        FileUtils.rm_f(css)
      end
    RUBY

    expect(lines).to eq(%w[true])
  end

  it "allow_pathを絞るとCSSのurl()も埋め込みに倒す" do
    lines = in_pipeline_app(<<~RUBY)
      Sghtmltopdf.configure { |c| c.allow_path = [File.join(root, "public")] }
      html = view.sghtmltopdf_stylesheet_link_tag("pipeline")
      puts html.include?("url(\\"data:image/png;base64,")
    RUBY

    expect(lines).to eq(%w[true])
  end

  it "allow_pathを絞ると読めなくなるので埋め込みに倒す" do
    lines = in_pipeline_app(<<~RUBY)
      Sghtmltopdf.configure { |c| c.allow_path = [File.join(root, "public")] }
      html = view.sghtmltopdf_image_tag("pipeline-logo.png")
      puts html.include?("data:image/png;base64,")
      # public/配下は相対パスのまま。
      puts view.sghtmltopdf_image_tag("logo.png") == %(<img src="logo.png">)
    RUBY

    expect(lines).to eq(%w[true true])
  end
end
