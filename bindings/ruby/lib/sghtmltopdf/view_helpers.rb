# frozen_string_literal: true

module Sghtmltopdf
  # Helpers for Action View.
  #
  # PDF rendering does not go through an HTTP server, so a URL such as `/assets/...` resolves
  # as a local file (`--base-url` defaults to `Rails.root/public`; see
  # [Railtie.default_options]). In production, with the assets precompiled, a plain
  # `stylesheet_link_tag` works as-is; but where the assets have not yet been written to
  # `public/`, as in development, it cannot resolve.
  #
  # So we provide helpers that inline the CSS into a `<style>`, as in
  #
  #   <%= sghtmltopdf_stylesheet_link_tag "pdf" %>
  #
  # (the equivalent of wicked_pdf's `wicked_pdf_stylesheet_link_tag`).
  module ViewHelpers
    # Return the asset's local file path, or `nil` if it is not found.
    #
    # It looks in this order:
    # 1. under `public/` (precompiled; production)
    #
    # 2. the asset pipeline's load paths (development; Propshaft or Sprockets)
    # The pipeline is consulted through `respond_to?` so neither gem becomes a dependency (best effort).
    def sghtmltopdf_asset_path(source)
      path = source.to_s
      return path if path.start_with?("/") && File.file?(path)

      from_public_dir(path) || from_asset_pipeline(path)
    end

    # Inline the CSS into a `<style>`. Several may be given, and any not found are skipped
    # silently (PDF generation itself is never stopped).
    def sghtmltopdf_stylesheet_link_tag(*sources)
      css = sources.flatten.filter_map do |source|
        path = sghtmltopdf_asset_path(with_extension(source, ".css"))
        File.read(path) if path
      end
      return "".html_safe if css.empty?

      content_tag(:style, css.join("\n").html_safe, type: "text/css")
    end

    # Replace `image_tag`'s src with a local file path.
    def sghtmltopdf_image_tag(source, options = {})
      image_tag(sghtmltopdf_asset_path(source) || source, options)
    end

    private

    # Take just the path part of the URL `asset_path` returns (which can carry an asset_host)
    # and map it to the real file under `public/`.
    def from_public_dir(source)
      url = respond_to?(:asset_path) ? asset_path(source) : source
      relative = url.to_s.sub(%r{\Ahttps?://[^/]+}, "").split(/[?#]/).first.to_s
      return nil if relative.empty?

      candidate = File.join(::Rails.public_path.to_s, relative)
      File.file?(candidate) ? candidate : nil
    end

    def from_asset_pipeline(source)
      assets = ::Rails.application.try(:assets)
      return nil if assets.nil?

      # Propshaft
      if assets.respond_to?(:load_path)
        found = assets.load_path.find(source)
        return found.path.to_s if found.respond_to?(:path)
      end
      # Sprockets
      if assets.respond_to?(:[])
        found = assets[source]
        return found.filename.to_s if found.respond_to?(:filename)
      end
      nil
    end

    def with_extension(source, extension)
      name = source.to_s
      name.end_with?(extension) ? name : "#{name}#{extension}"
    end
  end
end
