# frozen_string_literal: true

require "rbconfig"

module Sghtmltopdf
  module WindowsBuildEnvironment
    GNU_UCRT_ARCH = "x64-mingw-ucrt"
    LLVM_BIN = File.join(ENV.fetch("ProgramFiles", "C:/Program Files"), "LLVM", "bin")

    module_function

    # RubyInstaller ships the GNU/UCRT headers but its bundled libclang cannot parse
    # the newer intrinsic headers. Prefer a normal LLVM installation when present.
    def configure
      return unless RbConfig::CONFIG["arch"] == GNU_UCRT_ARCH

      llvm_bin = ENV.fetch("LIBCLANG_PATH", LLVM_BIN)
      llvm_bin = LLVM_BIN unless File.file?(File.join(llvm_bin, "libclang.dll"))

      if File.file?(File.join(llvm_bin, "libclang.dll"))
        ENV["LIBCLANG_PATH"] = llvm_bin
        paths = ENV.fetch("PATH", "").split(File::PATH_SEPARATOR)
        ENV["PATH"] = ([llvm_bin] + paths.reject { |path| path == llvm_bin }).join(File::PATH_SEPARATOR)
      end

      # bindgen otherwise assumes the host's MSVC ABI. Leave an explicitly supplied
      # --target in place (CI sets one) rather than prepending a duplicate; clang would
      # take the last --target anyway, but a single source keeps the args readable.
      existing_args = ENV.fetch("BINDGEN_EXTRA_CLANG_ARGS", "")
      return if existing_args.include?("--target=")

      bindgen_args = "--target=x86_64-w64-windows-gnu"
      ENV["BINDGEN_EXTRA_CLANG_ARGS"] = [bindgen_args, existing_args].reject(&:empty?).join(" ")
    end
  end
end
