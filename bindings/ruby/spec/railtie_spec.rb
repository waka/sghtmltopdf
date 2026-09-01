# frozen_string_literal: true

require "open3"

# Loading the Railtie is guarded by `defined?(Rails::Railtie)`.
# "The case where Rails is absent" cannot be created in the same process (other specs load
# Rails), so it is checked in a separate process.
RSpec.describe "the Railtie load guard" do
  ROOT = File.expand_path("..", __dir__)

  def ruby(script)
    out, err, status = Open3.capture3(
      RbConfig.ruby, "-I#{File.join(ROOT, "lib")}", "-rbundler/setup", "-e", script,
      chdir: ROOT
    )
    raise "the child process failed: #{err}" unless status.success?

    out.split("\n")
  end

  it "does not load the Railtie when Rails is absent" do
    expect(ruby(<<~RUBY)).to eq(%w[no no])
      require "sghtmltopdf"
      puts defined?(Rails) ? "yes" : "no"
      puts defined?(Sghtmltopdf::Railtie) ? "yes" : "no"
    RUBY
  end

  it "loads the Railtie when Rails is loaded" do
    expect(ruby(<<~RUBY)).to eq(%w[yes yes])
      require "rails"
      require "sghtmltopdf"
      puts defined?(Sghtmltopdf::Railtie) ? "yes" : "no"
      puts defined?(Sghtmltopdf::ViewHelpers) ? "yes" : "no"
    RUBY
  end

  it "still converts without Rails" do
    expect(ruby(<<~RUBY)).to eq(["%PDF-"])
      require "sghtmltopdf"
      puts Sghtmltopdf.render("<p>hello</p>")[0, 5]
    RUBY
  end
end
