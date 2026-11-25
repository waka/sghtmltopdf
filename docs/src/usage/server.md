# HTTPサーバモード

常駐させて、HTTP経由で変換を受け付けるモードです。
アプリケーションからプロセスを起動する必要がなくなり、負荷分散もロードバランサに任せられます。

```sh
sghtmltopdf server --listen 127.0.0.1:8080 --font NotoSansJP-Regular.ttf
# → 標準出力に `listening on 127.0.0.1:8080` が出る
```

```sh
curl --data-binary @invoice.html \
     'http://127.0.0.1:8080/pdf?page-size=A4&margin-top=20mm&toc' \
     -o invoice.pdf
```

常駐させるなら、日本語フォントを同梱した[Dockerイメージ](../getting-started/docker.md)が手軽です(引数なしでこのサーバとして起動します)。

```sh
docker run --rm -p 8080:8080 ghcr.io/waka/sghtmltopdf
```

## 起動オプション

| オプション | 既定 | 説明 |
|---|---|---|
| `--listen <ADDR:PORT>` | `127.0.0.1:8080` | 待ち受けアドレス。`:0`で空きポートを自動割り当て |
| `--workers <N>` | CPUコア数 | 同時に変換するワーカースレッド数 |
| `--max-queue <N>` | ワーカー数×4 | 受理待ちの上限。超えると503 |
| `--max-body-size <BYTES>` | 4194304 (4MiB) | リクエストボディの上限 |
| `--timeout <SECS>` | 30 | 1リクエストに与える秒数。キュー待ちと変換の合計(超えると504) |
| `--font <PATH>` ほかフォント指定 | | リクエストからは変更できない |
| `--enable-local-file-access` / `--allow <PATH>` / `--allow-remote-assets` | すべて禁止 | 明示的に許可する場合のみ |

> 認証とTLSは持ちません。
> 外部へ公開する場合はリバースプロキシを前段に置いてください。

## エンドポイント

| メソッド・パス | 説明 |
|---|---|
| `POST /pdf` | ボディのHTMLをPDFへ変換して返す(`application/pdf`) |
| `POST /pdf?stream=1` | 同上。chunked transfer encodingでページが確定したそばから流す |
| `GET /healthz` | `ok` |
| `GET /version` | `sghtmltopdf <version>` |

## クエリパラメータ

CLIのロングオプションから`--`を取った名前がそのまま使えます。
値の解釈もCLIと同一です(同じパーサへ通しているため)。

指定できるのは許可リストに載っているものだけです。
ページの体裁・PDFのメタデータ・ヘッダー/フッターの文字列・目次の見た目など、リクエストごとに変わってよく、かつサーバのファイルシステムにもネットワークにも触れないオプションが対象です。
それ以外は`400`を返します。

|クエリ|相当するCLIオプション|
|-|-|
|`?page-size=A4`|`--page-size A4`|
|`?margin-top=20mm`|`--margin-top 20mm`|
|`?toc`|`--toc`(値なしは真)|
|`?grayscale=1` / `=true`|`--grayscale`|
|`?grayscale=0` / `=false`|指定なしと同じ|

値はパーセントエンコードできます(`%XX`と`+`)。

各オプションの意味は[CLIリファレンス](./cli/reference.md)を参照してください。

### サーバ起動時にだけ指定できるオプション

以下は許可リストに含まれず、指定すると`400`を返します。
ローカルパスを取るもの・出力先・アクセス制御・ログ設定はサーバ起動時にだけ設定できます。

```
font, font-index, gothic-font, gothic-font-index, serif-font, serif-font-index,
mono-font, mono-font-index, output, cover, header-html, footer-html,
user-style-sheet, base-url, allow, enable-local-file-access,
disable-local-file-access, allow-remote-assets, log-level, quiet
```

## ファイルを添えて送る(multipart)

HTMLが参照するスタイルシート・画像・フォントがあるときは、`multipart/form-data`でHTMLと一緒に送れます。
`data:` URIとして埋め込む必要も、サーバのローカルファイルアクセスを開ける必要もありません。

```sh
curl 'http://127.0.0.1:8080/pdf?page-size=A4' \
     -F files=@index.html -F files=@style.css -F files=@logo.png \
     -o invoice.pdf
```

フォームの形は[Gotenberg](https://gotenberg.dev/)に合わせてあります。

| パート | 意味 |
|---|---|
| `files`(ファイル) | 送るファイル。いくつでも送れます。名前からフォルダ部分は捨てられるので、HTMLからは名前だけで参照します(`<img src="logo.png">`) |
| `index.html`という名前のファイル | 変換するHTML。必須です |
| `header.html`・`footer.html`という名前のファイル | ヘッダー・フッターのHTML(`--header-html`・`--footer-html`と同じ) |
| `cover.html`という名前のファイル | 表紙のHTML(`--cover`と同じ) |
| `user-style-sheet.css`という名前のファイル | ユーザーオリジンのCSS(`--user-style-sheet`と同じ)。UAスタイルより強く、HTML自身のCSSより弱い |
| ファイル名のないパート | オプション。名前と意味はクエリのキーと同じ(`page-size`など)で、許可リストも同じです |

`user-style-sheet.css`は、自分では書き換えないHTML(他システムの出力、HTMLメールなど)に、既定の見た目を足したいときに使います。
HTML自身のCSSのほうが強いので、HTMLが何も指定していないところだけに効きます。

たとえば、次の内容を`user-style-sheet.css`として送ります。

```css
body { font-size: 11pt; }
img  { max-width: 100%; }
```

```sh
curl http://127.0.0.1:8080/pdf \
     -F files=@index.html -F files=@user-style-sheet.css \
     -o out.pdf
```

* 送ったファイルはリクエストごとの専用ディレクトリに置かれ、変換が終わると消されます。相対参照の基準になり、読めるのはそのディレクトリの中だけです。`--enable-local-file-access`で起動した場合は、サーバの`--allow-path`で指定したディレクトリも読めます
  * `../`や絶対パスで読める範囲の外を参照しても読み込まれません(既定では無視され、`--load-media-error-handling abort`では失敗します)
* 同じ名前を2回送る(大文字小文字だけが違う名前も同じ扱い)、`..`やWindowsの予約名(`con`など)、`<>:"|?*`を含む名前は`400`です
* `--max-body-size`はファイルを含めたリクエスト全体に掛かります。画像やフォントを添えるなら引き上げてください
* `?stream=1`とも併せて使えます
* リモートの画像は、これまでどおり`--allow-remote-assets`で起動したときだけ読み込まれます

### アップロードを受け付けるサーバの運用

* ファイルはOSの一時ディレクトリ(Linuxでは`TMPDIR`、未指定なら`/tmp`)に置きます。ルートファイルシステムが読み取り専用のコンテナでは、書き込める`/tmp`か`TMPDIR`が必要です。無いと、アップロードはすべて`400`になります
* 一時ディレクトリには、最大で「ワーカー数 × `--max-body-size`」分のファイルが同時に置かれます。`/tmp`がtmpfsの場合は、その分のメモリを使います
* ディレクトリはリクエストの終了時に消しますが、後始末をせずにプロセスが終了した場合(SIGTERM、SIGKILL、OOM killer、abort)は`sghtmltopdf-upload-*`ディレクトリが残ります

## ステータスコード

| コード | 状況 |
|---|---|
| 200 | 成功(`Content-Type: application/pdf`) |
| 400 | 未知/禁止のクエリキー、値の形式不正、ボディが空、multipartの不備(`index.html`が無い、使えないファイル名など) |
| 404 | 未知のパス |
| 405 | 使えないメソッド |
| 413 | ボディが`--max-body-size`超過 |
| 415 | `multipart/form-data`以外のmultipart |
| 500 | レンダリング失敗 |
| 503 | キュー溢れ(`--max-queue`超過) |
| 504 | `--timeout`超過(キュー待ち、または変換が長すぎる) |

## ストリーミング

* 入力: リクエストボディは読み切らずに、64KiBずつエンジンへ流します。
  * 大きなHTMLを丸ごとメモリに載せません
* 出力: 既定はバッファ返却(`Content-Length`付き)。`?stream=1`を付けるとchunked transfer encodingで、ページが確定したそばから流します

```sh
curl --data-binary @big.html 'http://127.0.0.1:8080/pdf?stream=1' -o out.pdf
```

エンジン側の[ストリーミングモード](./cli/streaming.md)(`?streaming`)と併用すると、入力・レンダリング・出力のすべてが逐次処理になります。

## メモリの見積もり

変換に必要なメモリは、入力の大きさにほぼ比例します。
実測(最適化ビルド)では次のとおりでした。

| 要因 | 単価 | 抑えているもの |
|---|---|---|
| 要素の数 | 472B〜1210B/ノード | ノード数上限(50万) |
| テキストの量 | 約185MiB/入力1MiB | `--max-body-size` |

どちらの上限も、既定では最悪およそ600〜750MiBに収まる値にしてあります。
ワーカーは同時に変換するので、プロセス全体では「ワーカー数 × この値」が必要です。
既定(`--workers`はCPUコア数)のまま8コアの機械で動かすと最悪6GiB程度になるため、コンテナのメモリ制限に合わせて`--workers`か`--max-body-size`を調整してください。

要素数が上限を超えると`400`を返します。
`?streaming`を付けると処理済みの部分が随時解放されるため、要素数の上限には当たりにくくなります。

## 既知の限界

* `--timeout`はキュー待ちと変換の合計に効きます。変換の打ち切り判定はチャンク投入ごと・トップレベル要素ごと・ページ書き出しごとに行うため、超過に気づくのは最大でその1区間ぶん遅れます。レイアウトの1回の呼び出しの内側までは見ません
* 実測では、10MiBの重いHTMLに`--timeout 2`を指定した場合の実際の応答は2.1〜5.2秒でした(打ち切り後に大きなDOMを破棄する時間も含みます)。指定より早く返ることはありません
* `?stream=1`のとき、ヘッダ送信後に失敗してもステータスは200のままになります(パイプが閉じ、クライアントには不完全なPDFが届きます)。クエリの不正・空ボディ・サイズ超過はヘッダ送信前に検出するので400/413で返ります
* `?stream=1`のときは入力を先に読み切ります(HTTPライブラリの制約で、ボディ読み取りと応答が排他のため)。入力と出力のストリーミングは同時には使えません

## Railsから使う

Ruby gemには、変換をこのサーバへ委譲する`server_url`設定があります。
[Ruby / Rails](./ruby_rails.md#サーバへ委譲する)を参照してください。
