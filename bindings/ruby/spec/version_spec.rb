# frozen_string_literal: true

RSpec.describe "the version" do
  ROOT = File.expand_path("../../..", __dir__)

  def cargo_version(relative_path)
    path = File.join(ROOT, relative_path)
    skip "no #{relative_path} (it is invisible in a distributed gem)" unless File.exist?(path)

    # The first version line of the `[package]` section.
    File.read(path)[/^\s*version\s*=\s*"([^"]+)"/, 1]
  end

  it "keeps the Rust core and the gem at the same version" do
    expect(cargo_version("core/Cargo.toml")).to eq(Sghtmltopdf::VERSION)
  end

  it "keeps the native extension crate and the gem at the same version" do
    expect(cargo_version("bindings/ruby/ext/sghtmltopdf/Cargo.toml")).to eq(Sghtmltopdf::VERSION)
  end

  it "has a CHANGELOG" do
    skip "outside the repository" unless File.exist?(File.join(ROOT, "core"))

    expect(File.exist?(File.join(ROOT, "CHANGELOG.md"))).to be(true)
  end
end
