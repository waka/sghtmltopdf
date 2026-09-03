# frozen_string_literal: true

module Sghtmltopdf
  # Action View helpers.
  #
  # Rendering a PDF never goes through an HTTP server, so a URL such as
  # `/assets/…` is resolved as a local file (`--base-url` defaults to
  # `Rails.root/public`, see [Railtie.default_options]). In production, where
  # the assets are precompiled, that makes a plain `stylesheet_link_tag` work as
  # it is; in development, where nothing has been written to `public/` yet, it
  # cannot be resolved.
  #
  # Hence helpers that put the asset itself into the document:
  #
  #   <%= sghtmltopdf_stylesheet_link_tag "pdf" %>
  #   <%= sghtmltopdf_image_tag "logo.png" %>
  #
  # (the counterparts of wicked_pdf's `wicked_pdf_stylesheet_link_tag` and
  # `wicked_pdf_image_tag`).
  module ViewHelpers
    # Local file path of an asset, or `nil` when it cannot be found.
    #
    # Looks in
    #
    #   1. `public/` (precompiled; production)
    #   2. the asset pipeline load paths (development; Propshaft or Sprockets)
    #
    # in that order. The pipeline lookup goes through `respond_to?` so that
    # neither gem becomes a dependency (best effort).
    def sghtmltopdf_asset_path(source)
      path = source.to_s
      # A source that is already a URL (or a `data:` URI) is not an asset of
      # this application. Without this, `from_public_dir` would strip the host
      # off `https://example.com/logo.png` and hand back `public/logo.png`.
      return nil if path.match?(%r{\A(?:[a-z][a-z0-9+.\-]*:|//)}i)
      return path if path.start_with?("/") && File.file?(path)

      from_public_dir(path) || from_asset_pipeline(path)
    end

    # Expands the CSS itself into a `<style>`. Takes several sources; any that
    # cannot be found are skipped silently (rendering still goes ahead).
    def sghtmltopdf_stylesheet_link_tag(*sources)
      css = sources.flatten.filter_map do |source|
        path = sghtmltopdf_asset_path(with_extension(source, ".css"))
        File.read(path) if path
      end
      return "".html_safe if css.empty?

      content_tag(:style, css.join("\n").html_safe, type: "text/css")
    end

    # An `<img>` whose `src` the engine can read.
    #
    # By default the image is embedded as a `data:` URI, so nothing has to be
    # fetched at render time: no HTTP, and no dependence on `--base-url` or
    # `--allow`. It is also the only form that works in development, where the
    # file still sits outside `public/`. Passing the file path through
    # `image_tag` instead would hand Rails a filesystem path to turn into a URL,
    # which the engine cannot load (#44).
    #
    # `inline: false` emits a path relative to `base_url` instead, for a
    # document with so many images that base64 would bloat the HTML. It falls
    # back to embedding when the file is not under `base_url`, since a relative
    # path would not resolve from there.
    def sghtmltopdf_image_tag(source, options = {})
      options = options.dup
      inline = options.delete(:inline)
      path = sghtmltopdf_asset_path(source)
      # Not an asset of this application (a remote URL, say): leave it to Rails.
      return image_tag(source, options) if path.nil?

      if inline == false && (relative = relative_to_base_url(path))
        # `image_tag` would rewrite a relative source (prefixing `/images/`, the
        # asset host, and so on), so the tag is built directly. The options
        # become attributes as they are, which means the `size:` shorthand of
        # `image_tag` is not expanded into width and height here.
        return tag.img(**options.symbolize_keys.merge(src: relative))
      end
      # A `data:` URI matches `AssetUrlHelper::URI_REGEXP`, so `image_tag`
      # passes it through untouched and its own options keep working.
      image_tag(data_uri(path), options)
    end

    private

    # Takes the path part of the URL `asset_path` returns (which may carry an
    # asset host) and maps it onto a real file under `public/`.
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

    # `data:<type>;base64,<data>` for a local file. `pack("m0")` is strict
    # base64 (no line breaks) and needs no require, unlike `Base64`, which is no
    # longer a default gem on Ruby 3.4.
    def data_uri(path)
      base64 = [File.binread(path)].pack("m0")
      "data:#{mime_type_of(path)};base64,#{base64}"
    end

    # The path of `file` relative to the configured `base_url`, or `nil` when
    # `base_url` is not a directory holding it (it may be an http(s) URL, or the
    # file may live outside it, as an asset pipeline one in development does).
    def relative_to_base_url(file)
      base = Sghtmltopdf.config[:base_url].to_s
      return nil if base.empty? || base.match?(%r{\Ahttps?://}i)

      base = File.expand_path(base)
      prefix = base + File::SEPARATOR
      full = File.expand_path(file)
      full.start_with?(prefix) ? full.delete_prefix(prefix) : nil
    end

    # Media type from the file extension. The engine detects the format from the
    # bytes, so this only has to be honest, not exhaustive.
    def mime_type_of(path)
      MIME_TYPES.fetch(File.extname(path).downcase.delete_prefix("."), "application/octet-stream")
    end

    MIME_TYPES = {
      "png" => "image/png",
      "jpg" => "image/jpeg",
      "jpeg" => "image/jpeg",
      "gif" => "image/gif",
      "webp" => "image/webp",
      "avif" => "image/avif",
      "bmp" => "image/bmp",
      "ico" => "image/vnd.microsoft.icon",
      "tif" => "image/tiff",
      "tiff" => "image/tiff",
      "svg" => "image/svg+xml"
    }.freeze
    private_constant :MIME_TYPES

    def with_extension(source, extension)
      name = source.to_s
      name.end_with?(extension) ? name : "#{name}#{extension}"
    end
  end
end
