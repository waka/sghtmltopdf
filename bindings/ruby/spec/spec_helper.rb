# frozen_string_literal: true

require "sghtmltopdf"

Dir[File.join(__dir__, "support", "**", "*.rb")].sort.each { |file| require file }

# benchmark is no longer bundled in Ruby 4.0, so we measure it ourselves rather than adding a dependency.
def elapsed_seconds
  started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
  yield
  Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
end

# The PDF's `/CreationDate` and the trailer's `/ID` carry the time of the run (both are fixed
# length, so no other offset is affected). They are pinned before comparing bytes.
def normalize(pdf)
  pdf.gsub(/D:\d{14}Z/, "D:19700101000000Z")
     .gsub(/\/ID \[<\h{32}> <\h{32}>\]/, "/ID [<#{'0' * 32}> <#{'0' * 32}>]")
end

RSpec.configure do |config|
  config.expect_with(:rspec) { |c| c.syntax = :expect }
  config.disable_monkey_patching!
  config.order = :random
  Kernel.srand config.seed
end
