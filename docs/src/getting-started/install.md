# インストール

使い方に応じて3通りあります。

| 使い方 | 入れるもの |
|---|---|
| [HTTPサーバ](../usage/server.md)を常駐させる | Dockerイメージ`ghcr.io/waka/sghtmltopdf` |
| Ruby・Railsから使う | gem `sghtmltopdf` |
| 手元のコマンドラインで変換する | 実行ファイル`sghtmltopdf`(ソースからビルド) |

素の実行ファイル(GitHub Releasesのtarballやhomebrew)は配布していません。
サーバはイメージの中に、FFIから使う場合はgemの中にそれぞれ実行ファイル相当が入っているためです。
CLIを手元で試したい場合は下のソースビルドを使ってください。

## Docker

```sh
docker pull ghcr.io/waka/sghtmltopdf:latest
docker run --rm -p 8080:8080 ghcr.io/waka/sghtmltopdf
```

日本語フォント(BIZ UDPGothic / BIZ UDPMincho)を同梱しているので、フォントを用意しなくても日本語のPDFが出ます。
詳しくは[Docker](docker.md)を参照してください。

## ソースからビルド

必要なのは[Rustのstableツールチェイン](https://rustup.rs/)だけです。
C言語のライブラリやシステムパッケージへの依存はありません。

```sh
git clone https://github.com/waka/sghtmltopdf.git
cd sghtmltopdf
cargo build --release
```

実行ファイルは`target/release/sghtmltopdf`にできます。
パスの通った場所へ置くか、そのまま呼び出してください。

```sh
./target/release/sghtmltopdf --version
```

HTTPサーバモードが要らない場合は、featureを削って小さくできます。

```sh
cargo build --release --no-default-features --features cli
```

## Ruby / Rails

```ruby
# Gemfile
gem "sghtmltopdf"
```

ビルド済み(precompiled)のgemを配布する方針のため、利用側にRustのツールチェインは要りません。
対応は`x86_64-linux`・`aarch64-linux`・`x86_64-linux-musl`・`aarch64-linux-musl`・`arm64-darwin`と、Ruby 3.2以上です。

Gemfile.lockの`PLATFORMS`は`ruby`だけでかまいません。
bundlerがインストール先のプラットフォーム向けのprecompiled gemへ解決するため、`bundle lock --add-platform`は要りません。

外部プロセスは起動せず、ネイティブ拡張(magnus + rb-sys)として同じプロセスの中で変換します。
重い処理の間はGVLを解放するので、Pumaの他のスレッドは止まりません。

使い方は[Ruby / Rails](../usage/ruby_rails.md)を参照してください。

## フォントについて

`--font`を指定しない場合、システムにインストールされているフォントが使われます。
日本語を含む文書では、フォントファイルを明示するか、フォントを同梱した[Dockerイメージ](docker.md)を使うことを推奨します。

```sh
sghtmltopdf invoice.html \
  --font NotoSansJP-Regular.ttf \
  --gothic-font NotoSansJP-Regular.ttf
```

詳しくは[フォント](../supports/fonts.md)を参照してください。
