# frozen_string_literal: true

require_relative "../ext/sghtmltopdf/windows_build_environment"

# Pure logic, so it runs on every platform: the arch is stubbed rather than read from the host.
RSpec.describe Sghtmltopdf::WindowsBuildEnvironment do
  let(:llvm_bin) { described_class::LLVM_BIN }
  let(:libclang) { File.join(llvm_bin, "libclang.dll") }

  # Pretend to be the mingw-ucrt build host and start from a clean set of build env vars.
  def on_windows(libclang_present:)
    allow(RbConfig::CONFIG).to receive(:[]).and_call_original
    allow(RbConfig::CONFIG).to receive(:[]).with("arch").and_return(described_class::GNU_UCRT_ARCH)
    allow(File).to receive(:file?).and_call_original
    allow(File).to receive(:file?).with(libclang).and_return(libclang_present)

    saved = ENV.to_h.slice("LIBCLANG_PATH", "BINDGEN_EXTRA_CLANG_ARGS", "PATH")
    %w[LIBCLANG_PATH BINDGEN_EXTRA_CLANG_ARGS].each { |key| ENV.delete(key) }
    yield
  ensure
    %w[LIBCLANG_PATH BINDGEN_EXTRA_CLANG_ARGS PATH].each { |key| ENV.delete(key) }
    saved&.each { |key, value| ENV[key] = value }
  end

  it "does nothing on a non-mingw-ucrt arch" do
    allow(RbConfig::CONFIG).to receive(:[]).and_call_original
    allow(RbConfig::CONFIG).to receive(:[]).with("arch").and_return("x86_64-linux")
    before = ENV.to_h

    described_class.configure

    expect(ENV.to_h).to eq(before)
  end

  it "points bindgen at the LLVM install when libclang.dll is present" do
    on_windows(libclang_present: true) do
      ENV["PATH"] = "/usr/bin"

      described_class.configure

      expect(ENV["LIBCLANG_PATH"]).to eq(llvm_bin)
      # Prepended with the real separator (host-independent: the Windows drive path
      # contains a ':' that would otherwise be mistaken for a POSIX PATH separator).
      expect(ENV["PATH"]).to eq("#{llvm_bin}#{File::PATH_SEPARATOR}/usr/bin")
    end
  end

  it "forces the GNU target so bindgen does not assume the host MSVC ABI" do
    on_windows(libclang_present: true) do
      described_class.configure

      expect(ENV["BINDGEN_EXTRA_CLANG_ARGS"]).to eq("--target=x86_64-w64-windows-gnu")
    end
  end

  it "leaves the clang args alone when a --target is already set, instead of duplicating it" do
    on_windows(libclang_present: true) do
      ENV["BINDGEN_EXTRA_CLANG_ARGS"] = "--target=x86_64-w64-windows-gnu"

      described_class.configure

      expect(ENV["BINDGEN_EXTRA_CLANG_ARGS"]).to eq("--target=x86_64-w64-windows-gnu")
    end
  end

  it "does not set LIBCLANG_PATH when no libclang.dll is found" do
    on_windows(libclang_present: false) do
      described_class.configure

      expect(ENV).not_to have_key("LIBCLANG_PATH")
    end
  end
end
