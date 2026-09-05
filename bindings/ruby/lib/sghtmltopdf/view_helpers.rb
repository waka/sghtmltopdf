# frozen_string_literal: true

module Sghtmltopdf
  # Action View helpers.
  #
  # Rendering a PDF never goes through an HTTP server, so a URL such as
  # `/assets/…` is resolved as a local file (`--base-url` defaults to
  # `Rails.root/public`, see [Railtie.default_options]). In production, where
  # the assets are precompiled, that makes a plain `stylesheet_link_tag` work as
  # it is; in development, where nothing has been written to `public/` yet, it
  # cannot be resolved: the digest and the `/assets/` mount point are made up by
  # the pipeline at request time, so no file of that name is on disk.
  #
  # Hence helpers that look the asset up in the pipeline and hand the engine
  # something it can actually read:
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
    # The file is referenced by path, which keeps the HTML small: relative to
    # `base_url` when it sits under it (the precompiled case), and the absolute
    # filesystem path otherwise (the development case, where the file is still
    # in the pipeline's load path). The engine reads an absolute `src` as a
    # filesystem path once it fails to resolve under `base_url` (#44), so both
    # forms reach the same file.
    #
    # A path only works if the engine is allowed to read there, so it is used
    # only when `allow_path` (or `base_url`, with none set) covers the file.
    # Everything else falls back to a `data:` URI, which depends on no
    # configuration at all and therefore cannot fail: without that fallback a
    # blocked path would just disappear from the PDF, since a failed asset fetch
    # is ignored by default.
    #
    # `inline: true` forces embedding.
    def sghtmltopdf_image_tag(source, options = {})
      options = options.symbolize_keys
      inline = options.delete(:inline)
      path = sghtmltopdf_asset_path(source)
      # Not an asset of this application (a remote URL, say): leave it to Rails.
      return image_tag(source, options) if path.nil?

      if !inline && (src = engine_readable_src(path))
        # `image_tag` would rewrite the source (prefixing `/images/`, the asset
        # host, and so on), so the tag is built directly. That skips the one
        # option `image_tag` does more than copy, so `size:` is expanded here.
        return tag.img(**expand_size(options).merge(src: src))
      end
      # A `data:` URI matches `AssetUrlHelper::URI_REGEXP`, so `image_tag`
      # passes it through untouched and its own options keep working.
      image_tag(data_uri(path), options)
    end

    private

    # Takes the path part of the URL `asset_path` returns (which may carry an
    # asset host) and maps it onto a real file under `public/`.
    def from_public_dir(source)
      relative = asset_url_for(source).to_s.sub(%r{\Ahttps?://[^/]+}, "").split(/[?#]/).first.to_s
      return nil if relative.empty?

      candidate = File.join(::Rails.public_path.to_s, relative)
      File.file?(candidate) ? candidate : nil
    end

    # `asset_path` for `source`, falling back to `source` itself.
    #
    # Both pipelines raise rather than return a path for an asset outside their
    # load path (`Propshaft::MissingAssetError`,
    # `Sprockets::Rails::Helper::AssetNotFound`). A file that lives only in
    # `public/` is exactly that, so the raw source is tried against `public/`.
    def asset_url_for(source)
      return source unless respond_to?(:asset_path)

      asset_path(source)
    rescue StandardError
      source
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

    # The `src` to reference `file` by, or `nil` when the engine would not be
    # allowed to read it there.
    def engine_readable_src(file)
      return nil unless engine_can_read?(file)

      relative_to_base_url(file) || real_path(file)
    end

    # Whether the engine's rules for local files let it read `file`.
    #
    # `allow_path` decides on its own once it is set: the engine stops treating
    # `base_url` as the boundary and consults the allowed directories only.
    # A run that has no local access at all, or one delegated to a server that
    # may not even share this filesystem, can read nothing here.
    def engine_can_read?(file)
      config = Sghtmltopdf.config
      return false if config[:disable_local_file_access] || config[:server_url]

      file = real_path(file)
      return false if file.nil?

      dirs = Array(config[:allow_path]).filter_map { |dir| real_path(dir) }
      dirs = [real_path(config[:base_url])].compact if dirs.empty?
      dirs.any? { |dir| file.start_with?(dir + File::SEPARATOR) }
    end

    # The path of `file` relative to the configured `base_url`, or `nil` when
    # `base_url` is not a directory holding it (it may be an http(s) URL, or the
    # file may live outside it, as an asset pipeline one in development does).
    def relative_to_base_url(file)
      base = real_path(Sghtmltopdf.config[:base_url])
      full = real_path(file)
      return nil if base.nil? || full.nil?

      prefix = base + File::SEPARATOR
      full.start_with?(prefix) ? full.delete_prefix(prefix) : nil
    end

    # `path` with symlinks resolved, or `nil` when it is not a local path at
    # all. The engine canonicalizes both sides before comparing them, so a
    # symlink pointing out of an allowed directory does not count as inside it.
    def real_path(path)
      path = path.to_s
      return nil if path.empty? || path.match?(%r{\Ahttps?://}i)

      File.realpath(path)
    rescue SystemCallError
      nil
    end

    # `image_tag`'s `size:` shorthand: "40x30", or "40" for a square. The tag is
    # built without `image_tag` here, so the expansion has to happen here too.
    def expand_size(options)
      return options unless options.key?(:size)

      if options[:height] || options[:width]
        raise ArgumentError, "Cannot pass a :size option with a :height or :width option"
      end

      size = options[:size].to_s
      options = options.except(:size)
      case size
      when /\A(\d+(?:\.\d+)?)x(\d+(?:\.\d+)?)\z/ then options.merge(width: $1, height: $2)
      when /\A\d+(?:\.\d+)?\z/ then options.merge(width: size, height: size)
      else options
      end
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
