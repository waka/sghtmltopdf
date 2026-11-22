# Docker

日本語フォントを同梱した公式イメージを`ghcr.io`で配布しています。
サーバモードを常駐させる場合はこれが一番手軽です。

```sh
docker pull ghcr.io/waka/sghtmltopdf:latest
```

対応プラットフォームは`linux/amd64`と`linux/arm64`(どちらもglibc。Alpine等のmuslは対象外)です。
同じタグでどちらも引けます。

## タグ

リリース(`v*`のタグ)のたびに、gemと同じバージョンでイメージが公開されます。

| タグ | 内容 |
|---|---|
| `latest` | 最新のリリース |
| `0.8.0` | そのリリース |
| `0.8` | 0.8系の最新のパッチリリース |
| `edge` | `main`の最新(リリース前) |
| `sha-<コミット>` | そのコミット(`main`のものだけ) |

本番では`latest`や`edge`ではなく、`0.8`のようにバージョンを固定することをおすすめします。

## サーバとして使う

引数なしで起動すると[HTTPサーバ](../usage/server.md)になります。

```sh
docker run --rm -p 8080:8080 ghcr.io/waka/sghtmltopdf
```

```sh
curl --data-binary @invoice.html \
     'http://127.0.0.1:8080/pdf?page-size=A4' \
     -o invoice.pdf
```

コンテナの中では`--listen 0.0.0.0:8080`で待ち受けます。
サーバモードの既定は`127.0.0.1`(何も設定しないまま外部公開されるのを防ぐため)ですが、それだとコンテナの外から届かないので、イメージ側の`CMD`で明示しています。

起動オプションを変えたいときは、`server`から書き直します。

```sh
docker run --rm -p 8080:8080 ghcr.io/waka/sghtmltopdf \
    server --listen 0.0.0.0:8080 --workers 4 --max-body-size 52428800
```

> 認証とTLSは持ちません。
> 外部へ公開する場合はリバースプロキシを前段に置いてください。

### docker compose

```yaml
services:
  pdf:
    image: ghcr.io/waka/sghtmltopdf:0.8
    ports: ["8080:8080"]
    healthcheck:
      test: ["CMD", "sghtmltopdf", "--version"]
      interval: 30s
```

`curl`はイメージに入っていないので、ヘルスチェックは`--version`で代用するか、外側(ロードバランサ等)から`GET /healthz`を叩いてください。

RubyのアプリからこのサーバへPDF変換を任せる設定は、[Ruby / RailsのDockerで使う](../usage/ruby_rails.md#dockerで使う)にあります。

## CLIとして使う

`ENTRYPOINT`が実行ファイルそのものなので、引数を渡せばCLIとして動きます。

```sh
docker run --rm -v "$PWD:/work" -w /work --user "$(id -u):$(id -g)" \
    ghcr.io/waka/sghtmltopdf invoice.html -o invoice.pdf
```

コンテナはrootでは動きません(UID 10001)。
ホストのディレクトリへPDFを書き出すときは、上のように`--user`でホスト側の所有者に合わせてください。

## 同梱しているフォント

[BIZ UDPGothic](https://fonts.google.com/specimen/BIZ+UDPGothic)と[BIZ UDPMincho](https://fonts.google.com/specimen/BIZ+UDPMincho)のRegular・Bold(計4本、SIL Open Font License 1.1)が入っています。
ライセンス全文はイメージ内の`/usr/share/doc/sghtmltopdf/fonts/`にあります。

| CSSの指定 | 使われるフォント |
|---|---|
| `font-family`未指定 | BIZ UDPMincho(明朝) |
| `font-family: sans-serif` | BIZ UDPGothic(ゴシック) |
| `font-family: serif` | BIZ UDPMincho |
| `font-family: monospace` | 等幅フォントは同梱していないため、BIZ UDPMinchoにフォールバック |
| `font-weight: bold` | 各書体のBold(合成太字ではありません) |

フォントが固定されているので、同じHTMLからは同じPDFが出ます(PDF内の作成日時を除く)。
ホストのフォント構成に出力が左右されないのがイメージを使う利点のひとつです。

別のフォントを使いたい場合は、マウントして[`--font`](../supports/fonts.md)で渡してください。
同梱フォントより優先されます。

```sh
docker run --rm -v "$PWD:/work" -w /work --user "$(id -u):$(id -g)" \
    ghcr.io/waka/sghtmltopdf invoice.html -o invoice.pdf \
    --font fonts/YourFont-Regular.ttf --gothic-font fonts/YourFont-Regular.ttf
```

## イメージの中身

|パス|内容|
|-|-|
|`/usr/local/bin/sghtmltopdf`|実行ファイル|
|`/usr/share/fonts/truetype/sghtmltopdf/*.ttf`|同梱フォント|
|`/usr/share/doc/sghtmltopdf/fonts/OFL-*.txt`|フォントのライセンス|
|`/work`|既定の作業ディレクトリ|

ベースは`debian:bookworm-slim`で、追加のシステムパッケージはありません(TLSのルート証明書は実行ファイルに埋め込まれているため`ca-certificates`も不要です)。
