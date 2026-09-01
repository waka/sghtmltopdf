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

  # `gem build` can only collect files under the directory holding the gemspec,
  # so LICENSE is a copy of the one at the repository root.
  spec.files = Dir[
    "lib/**/*.rb",
    "ext/**/*.{rb,rs,toml}",
    "Cargo.{toml,lock}",
    "LICENSE*",
    "README*"
  ]
  spec.require_paths = ["lib"]
  spec.extensions = ["ext/sghtmltopdf/extconf.rb"]
end
