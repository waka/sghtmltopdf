# Rustから

エンジンは[crates.io](https://crates.io/crates/sghtmltopdf)に`sghtmltopdf`として公開しています。
CLIの実行ファイルを作っているのと同じクレートです。
APIの一覧は[docs.rs](https://docs.rs/sghtmltopdf)を参照してください。

```sh
cargo add sghtmltopdf
```

## Converter

`Converter`はCLIと同じオプション列を受け取ります。
[オプションリファレンス](cli/reference.md)にあるものは、同じ名前・同じ意味のまま使えます。

```rust,noplayground
use sghtmltopdf::{with_render_stack, Converter};

let converter = Converter::from_args(["--page-size", "A4", "--margin-top", "20mm"])?;
let html = std::fs::File::open("invoice.html")?;
let pdf = with_render_stack(|| converter.render_to_vec(html))?;
std::fs::write("invoice.pdf", pdf)?;
```

オプションは`from_args`の時点で一度だけ検証します。
作った`Converter`は何度でも使い回せ、`Send`なので別スレッドへ渡せます。

入力パス・`--output`・`server`サブコマンドは指定できません。
HTMLは`render`に渡すReaderから読み、PDFは`render`に渡すSinkへ書くためです。

文字コードはブラウザと同じ順(BOM、`<meta charset>`、UTF-8)で判定します。
`--encoding`を指定した場合はそちらを使います。

### スタックサイズ

レンダリングは文書の入れ子の深さだけ再帰します。
スレッドの既定のスタックでは深い文書で落ちることがあるため、`with_render_stack`を通して十分なスタック(16MiB)を持つスレッドで実行してください。
クロージャの戻り値はそのまま返り、panicも呼び出し元へ伝わります。

### エラー

`ConvertError`はCLIの終了コードと同じ4分類です。

| バリアント | 終了コード | 例 |
|---|---|---|
| `Usage` | 1 | 知らないオプション、値の形式違い、実装しないオプション |
| `Input` | 2 | ファイルがない、フォントを読めない、書き込みに失敗した |
| `Render` | 3 | エンジンの上限(入れ子の深さなど)を超えた |
| `Timeout` | 4 | 制限時間を超えた(HTTPサーバモードのみ) |

`ConvertError`は`#[non_exhaustive]`なので、`match`には`_`の腕が必要です。

## ページが確定したそばから書き出す

`render_to_vec`はPDF全体をメモリに集めてから返します。
`render`に自前の`Sink`を渡すと、ページのレイアウトが確定するたびにそのバイト列を受け取れます。

エンジンは`write`を先頭から順に(ページが確定するたびに1回以上)呼び、最後に`finish`を1回だけ呼びます。
`finish`の戻り値が`render`の戻り値になります。

```rust,noplayground
use std::io::{self, Write};
use sghtmltopdf::{with_render_stack, Converter, Sink};

struct WriterSink<W: Write>(W);

impl<W: Write> Sink for WriterSink<W> {
    type Output = W;
    type Error = io::Error;

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }

    fn finish(mut self) -> io::Result<W> {
        self.0.flush()?;
        Ok(self.0)
    }
}

let converter = Converter::from_args(["--page-size", "A4"])?;
let html = std::fs::File::open("invoice.html")?;
let socket = std::net::TcpStream::connect("127.0.0.1:9000")?;
with_render_stack(|| converter.render(html, WriterSink(socket)))?;
```

`Converter::render`に渡すSinkは、`Error`が`io::Error`である必要があります。

次のSinkは最初から用意しています。

| Sink | 書き出し先 |
|---|---|
| `MemorySink` | メモリ。`finish`でバイト列を返す |
| `FileSink` | ファイル。一時ファイルに書き、成功したときだけ出力パスへrenameする |
| `StdoutSink` | 標準出力 |
| `BufferedSink` | 指定したバイト数ごとにコールバックへ渡す。S3のマルチパートアップロード(最後以外は5MB以上)向け |

ストリーミングモード(`--streaming`)と組み合わせると、ページを書き出した後にそのメモリを解放しながら読み進めます。
使える機能の制約は[ストリーミングモード](cli/streaming.md)を参照してください。

## Engine

`Engine`はより低レベルなAPIです。
`EngineOptions`を組み立て、HTMLをチャンクごとに`feed`し、最後に`finish`します。

```rust,noplayground
use sghtmltopdf::{Engine, EngineOptions, FontSpec, MemorySink, Mode, PageSize};

let mut options = EngineOptions::default();
options.mode = Mode::Streaming;
options.settings.size = PageSize::A4;
options.fonts = vec![FontSpec { path: "fonts/NotoSansJP-Regular.ttf".into(), index: 0 }];

let mut engine = Engine::new(options, MemorySink::new());
engine.feed(b"<!DOCTYPE html><p>Hello</p>")?;
let pdf: Vec<u8> = engine.finish()?;
```

`EngineOptions`は`#[non_exhaustive]`なので、クレートの外では構造体リテラルで書けません。
`EngineOptions::default()`で作ってからフィールドを書き換えてください。

文字コードの判定、`--header-center`などの簡易オプションから`@page`への変換、ヘッダー/フッターHTMLの読み込みは`Converter`の側で行っています。
`Engine`は受け取ったバイト列をUTF-8として扱います。

ローカルファイルへのアクセスも既定が異なります。
`EngineOptions`の既定は制限なしで、CLIのように基準ディレクトリの外を拒否しません。
信頼できないHTMLを扱う場合は`local_access`を設定してください。

特に理由がなければ`Converter`を使ってください。

## feature

| feature | 既定 | 内容 |
|---|---|---|
| `cli` | 有効 | `sghtmltopdf`コマンドと`Converter`(clap) |
| `server` | 有効 | `sghtmltopdf server`(tiny_http) |
| `svg` | 有効 | SVG画像をベクタのまま埋め込む(svg2pdf) |
| `svg-text` | 無効 | SVG画像の中の`<text>` |

ライブラリとして使う場合は、HTTPサーバを外すと依存が減ります。

```toml
[dependencies]
sghtmltopdf = { version = "0.5", default-features = false, features = ["cli", "svg"] }
```

## 安定性

semverの対象は、クレートのルートから使える型だけです。
`sghtmltopdf::layout`のようなモジュールはドキュメントに出していません。
テストやRubyバインディングのために公開しているだけで、どのリリースでも変わることがあります。
