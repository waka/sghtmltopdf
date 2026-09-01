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

puts "ok: sghtmltopdf #{Sghtmltopdf::VERSION} / ruby #{RUBY_VERSION} #{RUBY_PLATFORM} / #{pdf.bytesize} bytes"
