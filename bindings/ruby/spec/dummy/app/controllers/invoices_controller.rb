# frozen_string_literal: true

# `render pdf:`の結合テスト用。
class InvoicesController < ActionController::Base
  # 既定のテンプレート(invoices/show)をPDFにする。
  def show
    render pdf: "invoice"
  end

  # ファイル名・添付・変換オプションの受け渡し。
  def download
    render pdf: "invoice",
      template: "invoices/show",
      filename: "請求書.pdf",
      disposition: "attachment",
      page_size: "A5"
  end

  # レイアウトを指定する(wicked_pdfからの移行で使われるキー)。
  def with_layout
    render pdf: "invoice", template: "invoices/show", layout: "pdf"
  end

  # PDFにせずHTMLのまま返すデバッグ用オプション。
  def as_html
    render pdf: "invoice", template: "invoices/show", show_as_html: true
  end

  # `public/`のCSSを`<style>`へ展開するビューヘルパ。
  def with_stylesheet
    render pdf: "invoice", template: "invoices/with_stylesheet"
  end

  # Embeds a local image through the view helper.
  def with_image
    render pdf: "invoice", template: "invoices/with_image"
  end

  # `examples/receipt.html`をそのままビューにしたもの。CLIの出力と
  # 突き合わせるために使う(CSSは`<link>`のまま。`public/main.css`を
  # `--base-url`経由で解決する)。
  def receipt
    render pdf: "receipt", template: "invoices/receipt"
  end

  # 未知のオプションはclapが弾く(Sghtmltopdf::UsageError)。
  def bad_option
    render pdf: "invoice", template: "invoices/show", no_such_option: "x"
  end
end
