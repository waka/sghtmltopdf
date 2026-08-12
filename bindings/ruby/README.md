# sghtmltopdf

Ruby binding for [sghtmltopdf](https://github.com/waka/sghtmltopdf), an HTML-to-PDF renderer written in Rust that does not depend on Chromium, WebKit, or Gecko.

The engine runs inside your process through a native extension (magnus + rb-sys) — no subprocess, no temporary files — and releases the GVL while rendering, so other Puma threads keep running.

[Documentation](https://waka.github.io/sghtmltopdf/en/usage/ruby_rails.html) · [Repository](https://github.com/waka/sghtmltopdf) · [CHANGELOG](https://github.com/waka/sghtmltopdf/blob/main/CHANGELOG.md)

## Install

```ruby
# Gemfile
gem "sghtmltopdf"
```

Precompiled native gems are published for `x86_64-linux`, `aarch64-linux`, `x86_64-linux-musl`, `aarch64-linux-musl`, and `arm64-darwin`.
There is no build step on those platforms, and no Rust toolchain is required.

A `Gemfile.lock` that lists only the `ruby` platform is all you need.
Bundler resolves that entry to the precompiled gem for whichever platform it is installing on, so `bundle lock --add-platform` is not necessary and the lockfile stays byte-identical whether you install on an Apple Silicon laptop or deploy to `x86_64-linux` or Alpine.

Elsewhere (Intel Mac, Windows) there is no precompiled gem, and bundler falls back to the `ruby` platform gem.
That gem is a placeholder: it installs without building anything — so `bundle install` succeeds rather than dying on a compile — but it carries no renderer and raises a `LoadError` explaining the situation when required.
To render from such an environment, run the renderer as a separate `sghtmltopdf server` process (the [Docker image](https://waka.github.io/sghtmltopdf/en/getting-started/docker.html) does this) and call it over HTTP with any HTTP client; this gem itself cannot be loaded there.

Requires Ruby >= 3.2.

## Usage

```ruby
pdf = Sghtmltopdf.render("<h1>Invoice</h1>", page_size: "A4", margin_top: "20mm")
```

Option names are the CLI long options without `--` and with `-` replaced by `_`, so `--page-size A4` becomes `page_size: "A4"`.
The [option reference](https://waka.github.io/sghtmltopdf/en/usage/cli/reference.html) lists all of them.

Write straight to a file (written to a temporary file and renamed on success, so a failure never leaves a broken PDF behind), or take the bytes in chunks:

```ruby
Sghtmltopdf.render_to_file(html, "invoice.pdf", page_size: "A4")

Sghtmltopdf.render(html) { |bytes| io.write(bytes) }
```

## Rails

Adding the gem is enough; the Railtie wires everything up, and nothing is loaded when Rails is absent.

```ruby
# config/initializers/sghtmltopdf.rb
Sghtmltopdf.configure do |c|
  c.page_size   = "A4"
  c.gothic_font = Rails.root.join("vendor/fonts/NotoSansJP-Regular.ttf")
end
```

A `:pdf` renderer is registered, in the spirit of [wicked_pdf](https://github.com/mileszs/wicked_pdf) — the same keys, so an existing controller often needs no change at all:

```ruby
class InvoicesController < ApplicationController
  def show
    render pdf: "invoice",              # filename; ".pdf" is appended
      template: "invoices/show",
      layout: "pdf",
      page_size: "A4", margin_top: "20mm"
  end
end
```

View-rendering keys (`template`, `layout`, `locals`, …) go to `render_to_string`, response keys (`filename`, `disposition`, `status`) go to `send_data`, `show_as_html: true` returns the HTML instead of a PDF, and everything else is passed to the converter.
Converter keys are flat CLI flag names, so wicked_pdf's nested `margin: {top: 10}` becomes `margin_top: "10mm"` (with the unit spelled out); the [migration guide](https://waka.github.io/sghtmltopdf/en/migration/wicked-pdf.html) maps every key one by one.

### Assets

PDF rendering does not go through the HTTP server, so `/assets/…` URLs are resolved as local files: the Railtie defaults `base_url` to `Rails.root/public` and restricts local reads to `Rails.root` via `allow`.
That is enough for a precompiled production app; in development, these helpers inline the asset instead:

```erb
<%= sghtmltopdf_stylesheet_link_tag "pdf" %>
<%= sghtmltopdf_image_tag "logo.png" %>
```

### Streaming the response

To send pages as soon as their layout is final, pass a block and use `ActionController::Live` — this also makes `Rack::Timeout` and `Thread#kill` effective at chunk boundaries:

```ruby
class InvoicesController < ApplicationController
  include ActionController::Live

  def show
    response.headers["Content-Type"] = "application/pdf"
    html = render_to_string(template: "invoices/show", layout: "pdf")
    Sghtmltopdf.render(html) { |bytes| response.stream.write(bytes) }
  ensure
    response.stream.close
  end
end
```

## Delegating to a server

If you would rather not spend the app's CPU on rendering, or you want one renderer shared by several apps, set `server_url` and the same calls are delegated over HTTP to a separate `sghtmltopdf server` process.

```ruby
Sghtmltopdf.configure { |c| c.server_url = "http://pdf:8080" }
```

The [official Docker image](https://waka.github.io/sghtmltopdf/en/getting-started/docker.html) runs that server and bundles Japanese fonts.

## License

MIT License ([LICENSE](LICENSE)).
