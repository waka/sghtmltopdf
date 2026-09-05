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
    #
    # The file is not copied verbatim: its `url()`s are pointed at files the
    # engine can read and its `@import`s are spliced in, see
    # [#inline_stylesheet].
    def sghtmltopdf_stylesheet_link_tag(*sources)
      css = sources.flatten.filter_map do |source|
        path = sghtmltopdf_asset_path(with_extension(source, ".css"))
        inline_stylesheet(path) if path
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

    # `path` read as CSS, with every `url()` pointed at a file the engine can
    # read and every `@import` spliced in.
    #
    # Both have to happen here because the engine keeps one base for the whole
    # document: every CSS source is concatenated before it is parsed, so a
    # `url()` is resolved against the HTML's `base_url` whatever stylesheet it
    # came from. Only this side knows where each file sits on disk, so each
    # one's references are resolved against its own directory before its text
    # is spliced into the next.
    #
    # What has to be undone is mostly the asset pipeline's own work: it
    # rewrites every `url()` through `asset_path` while precompiling, which
    # yields a digested `/assets/…` path, or an absolute URL once `asset_host`
    # is set. Rendering never goes through the HTTP server, so neither can be
    # fetched.
    #
    # `chain` holds the files already being expanded, innermost last.
    def inline_stylesheet(path, chain = [])
      css = File.read(path)
      dir = File.dirname(path)
      chain += [real_path(path)].compact
      comments = comment_ranges(css)
      out = +""
      cursor = 0
      while (import = IMPORT_STATEMENT.match(css, cursor))
        commented = comments.any? { |range| range.cover?(import.begin(0)) }
        expanded = imported_stylesheet(import[:href], dir, chain) unless commented
        out << rewrite_css_urls(css[cursor...import.begin(0)].to_s, dir)
        cursor = import.end(0)
        out << (expanded || import[0])
      end
      out << rewrite_css_urls(css[cursor..].to_s, dir)
    end

    # The expanded content of an `@import` target, or `nil` to leave the
    # statement as it is, which hands it to the engine untouched. That happens
    # when it names no asset of this application, when the nesting is too deep,
    # and when it points back at a file already being expanded.
    #
    # Media conditions on the statement are dropped. The engine replaces the
    # whole statement with the imported text too, so nothing changes by doing
    # it here.
    def imported_stylesheet(href, dir, chain)
      return nil if chain.length >= MAX_IMPORT_DEPTH

      target = css_url_target(unquote(href), dir)
      return nil if target.nil?

      real = real_path(target)
      return nil if real.nil? || chain.include?(real)

      inline_stylesheet(target, chain)
    end

    # Byte ranges of the `/* … */` comments in `css`. A commented-out `@import`
    # must not be spliced in.
    def comment_ranges(css)
      css.enum_for(:scan, CSS_COMMENT).map { Regexp.last_match.begin(0)...Regexp.last_match.end(0) }
    end

    # Every `url()` in `css` pointed at something the engine can read: a path
    # when it may read the file there, a `data:` URI when it may not.
    #
    # A reference that names no file of this application is left alone, which
    # covers `data:` URIs, bare fragments, and genuinely remote resources such
    # as a font served by a CDN.
    def rewrite_css_urls(css, dir)
      css.gsub(CSS_URL) do
        match = Regexp.last_match
        target = css_url_target(unquote(match[:href]), dir)
        target ? css_url(engine_readable_src(target) || data_uri(target)) : match[0]
      end
    end

    # The local file a CSS reference names, or `nil` when it names none.
    def css_url_target(ref, dir)
      path = ref.split(/[?#]/, 2).first.to_s
      return nil if path.empty? || path.match?(/\Adata:/i)

      if path.match?(%r{\A(?:[a-z][a-z0-9+.\-]*:|//)}i)
        # Only an http(s) or protocol-relative URL can be one of ours. The host
        # is not compared against `asset_host`: it may be a callable or carry a
        # `%d` wildcard, so there is no general way to recognise it. Finding the
        # path on disk is the test instead, and what that finds is the very file
        # the reference names.
        return nil unless path.match?(%r{\A(?:https?:)?//}i)

        path = path.sub(%r{\A(?:https?:)?//[^/]*}i, "")
        return nil if path.empty?
      end

      path.start_with?("/") ? from_site_root(path) : from_stylesheet_dir(path, dir)
    end

    # A site-root-relative reference (`/assets/pdf-<digest>.css`) mapped onto a
    # file. `relative_url_root` comes off first: the pipeline writes it in front
    # of every URL, but it is a mount point, not a directory under `public/`.
    def from_site_root(path)
      root = ::Rails.application.config.try(:relative_url_root).to_s
      path = path.delete_prefix(root) unless root.empty?
      candidate = File.join(::Rails.public_path.to_s, path)
      return candidate if File.file?(candidate)

      # Nothing is precompiled in development, so the load path is asked as
      # well. The mount point is not part of a logical path either.
      logical = path.delete_prefix("/")
      prefix = ::Rails.application.config.try(:assets)&.prefix.to_s.delete_prefix("/")
      from_asset_pipeline(logical) ||
        (prefix.empty? ? nil : from_asset_pipeline(logical.delete_prefix("#{prefix}/")))
    end

    # A reference relative to the stylesheet that wrote it, which is what CSS
    # says it means and what the pipeline assumed while compiling the file.
    #
    # The load path is tried too. An uncompiled stylesheet sitting at the root
    # of its own load path names assets by logical path, which is spelled the
    # same way but is looked up across every root.
    def from_stylesheet_dir(path, dir)
      candidate = File.expand_path(path, dir)
      return candidate if File.file?(candidate)

      from_asset_pipeline(path.delete_prefix("./"))
    end

    # A `url()` whose argument is always quoted, so that a path holding a space
    # or a parenthesis survives.
    def css_url(value)
      %(url("#{value.gsub(/["\\]/) { |char| "\\#{char}" }}"))
    end

    def unquote(value)
      value = value.to_s.strip
      quoted = value.match(/\A(["'])(.*)\1\z/m)
      quoted ? quoted[2] : value
    end

    # Matches the engine's own cap (`core/src/style/import.rs`). Past it the
    # statement is left in place and the engine decides what to do with it.
    MAX_IMPORT_DEPTH = 16
    private_constant :MAX_IMPORT_DEPTH

    # `@import` up to its terminating `;`, in both the `url()` and the bare
    # string form. Media conditions are matched so they are consumed, not kept.
    IMPORT_STATEMENT = /
      @import \s+
      (?: url\( \s* (?<href> "[^"]*" | '[^']*' | [^)"'\s]* ) \s* \)
        | (?<href> "[^"]*" | '[^']*' ) )
      [^;]* ;
    /xi
    private_constant :IMPORT_STATEMENT

    # A `url()` token. The lookbehind keeps identifiers ending in "url" out.
    CSS_URL = /(?<![\w-])url\( \s* (?<href> "[^"]*" | '[^']*' | [^)"'\s]* ) \s* \)/xi
    private_constant :CSS_URL

    CSS_COMMENT = %r{/\*.*?\*/}m
    private_constant :CSS_COMMENT

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
      "svg" => "image/svg+xml",
      # Reached through a stylesheet's `url()`, not through `image_tag`.
      "otf" => "font/otf",
      "ttf" => "font/ttf",
      "ttc" => "font/collection",
      "woff" => "font/woff",
      "woff2" => "font/woff2",
      "eot" => "application/vnd.ms-fontobject",
      "css" => "text/css"
    }.freeze
    private_constant :MIME_TYPES

    def with_extension(source, extension)
      name = source.to_s
      name.end_with?(extension) ? name : "#{name}#{extension}"
    end
  end
end
