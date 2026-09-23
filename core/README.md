# sghtmltopdf

An HTML-to-PDF renderer like wkhtmltopdf, written in Rust, that does not depend on Chromium, WebKit or Gecko.

[Documentation](https://waka.github.io/sghtmltopdf/en/) · [Repository](https://github.com/waka/sghtmltopdf)

It is aimed at documents that flow top to bottom with explicit breaks (invoices, receipts, reports) rather than at rendering arbitrary web pages.

* No browser process. One binary, or a library inside your own process.
* Fonts, including `@font-face` webfonts, are resolved during rendering, so there is nothing to wait for.
* Streaming. HTML is read in chunks and each page is written out as soon as its layout is final.
* Page breaks are first class. CSS Fragmentation (`break-before`, `break-inside`, `orphans`, `widows`) and `@page` are implemented directly.

## Command line

```sh
cargo install sghtmltopdf
sghtmltopdf invoice.html -o invoice.pdf --page-size A4 --margin-top 20mm
```

Most flags keep the name and meaning they have in wkhtmltopdf.
`sghtmltopdf server` starts an HTTP server that takes the same options as a query string.
See the [option reference](https://waka.github.io/sghtmltopdf/en/usage/cli/reference.html).

## Library

`Converter` takes the same options as the command, which is the simplest way to embed it:

```rust,no_run
use sghtmltopdf::{with_render_stack, Converter};

let converter = Converter::from_args(["--page-size", "A4", "--title", "Invoice"])?;
let html = std::fs::File::open("invoice.html")?;
let pdf = with_render_stack(|| converter.render_to_vec(html))?;
std::fs::write("invoice.pdf", pdf)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`Engine` is the lower-level API: build `EngineOptions`, `feed` it HTML in chunks and `finish` it, with the PDF written to any `Sink` as each page is completed.
Rendering recurses as deep as the document, so run it on a thread with a large enough stack; `with_render_stack` does that for you.

## Features

| Feature | Default | |
|---|---|---|
| `cli` | yes | The `sghtmltopdf` binary and `Converter` (clap) |
| `server` | yes | `sghtmltopdf server` (tiny_http) |
| `svg` | yes | SVG images embedded as vectors (svg2pdf) |
| `svg-text` | no | `<text>` inside SVG images |

For library use, `default-features = false` drops the command line and the HTTP server.

## License

MIT
