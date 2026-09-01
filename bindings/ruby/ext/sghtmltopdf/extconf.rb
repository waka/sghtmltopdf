# frozen_string_literal: true

require "mkmf"
require "rb_sys/mkmf"

core = File.expand_path("../../../../core", __dir__)
unless File.exist?(File.join(core, "Cargo.toml"))
  abort <<~MESSAGE
    sghtmltopdf: the Rust core (#{core}) was not found.

    This gem is distributed as a precompiled gem for the supported platforms.
    No prebuilt gem exists for your environment (#{RUBY_PLATFORM} / ruby #{RUBY_VERSION}),
    so a build from source was attempted, but the source gem does not include the
    Rust core and cannot be built.

    Supported platforms: x86_64-linux / aarch64-linux / x86_64-linux-musl / aarch64-linux-musl / arm64-darwin
  MESSAGE
end

# Built as `lib/sghtmltopdf/sghtmltopdf.so`.
create_rust_makefile("sghtmltopdf/sghtmltopdf")
