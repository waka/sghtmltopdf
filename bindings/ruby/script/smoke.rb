# frozen_string_literal: true

# Checks that an installed gem actually works.
# Run this after `gem install` of a precompiled gem, without using the repository's lib (no `bundle exec`, no `-Ilib`).

require "sghtmltopdf"

pdf = Sghtmltopdf.render(<<~HTML, page_size: "A4", margin_top: "20mm")
  <html><head><title>smoke</title></head>
  <body><h1>sghtmltopdf</h1><p>precompiled gem smoke test</p></body></html>
HTML

abort "not a PDF: #{pdf[0, 20].inspect}" unless pdf.start_with?("%PDF-")
abort "the PDF is not terminated" unless pdf.end_with?("%%EOF")
abort "the PDF is too small: #{pdf.bytesize} bytes" if pdf.bytesize < 500

require "tmpdir"
Dir.mktmpdir do |dir|
  path = File.join(dir, "smoke.pdf")
  Sghtmltopdf.render_to_file("<p>file</p>", path)
  abort "nothing was written to the file" unless File.binread(path).start_with?("%PDF-")
end

Dir.mktmpdir do |dir|
  base_dir = File.join(dir, "base")
  asset_dir = File.join(dir, "assets")
  Dir.mkdir(base_dir)
  Dir.mkdir(asset_dir)

  # Keep the asset outside base_url so only allow_path permits reading it.
  image_path = File.join(asset_dir, "logo.png").tr("\\", "/")
  # The one-pixel PNG used in core/tests/base_href.rs.
  png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==".unpack1("m0")
  File.binwrite(image_path, png)
  html = %(<img src="#{image_path}" width="20" height="20">)

  allowed = Sghtmltopdf.render(html, base_url: base_dir, allow_path: [asset_dir])
  blocked = Sghtmltopdf.render(html, base_url: base_dir)
  abort "absolute image path was not embedded with allow_path: #{image_path}" unless allowed.include?("/Subtype /Image")
  abort "absolute image path was embedded without allow_path: #{image_path}" if blocked.include?("/Subtype /Image")
end

puts "ok: sghtmltopdf #{Sghtmltopdf::VERSION} / ruby #{RUBY_VERSION} #{RUBY_PLATFORM} / #{pdf.bytesize} bytes"
