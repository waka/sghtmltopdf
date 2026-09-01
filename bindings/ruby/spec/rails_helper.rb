# frozen_string_literal: true

# For the Rails integration tests. It boots a minimal application with `spec/dummy` as
# `Rails.root`.
ENV["RAILS_ENV"] ||= "test"

require "logger"
require "rails"
require "action_controller/railtie"
require "rack/test"

# In an ordinary Rails application, Bundler.require runs after `rails/all`, so the Railtie is
# loaded by the guard in `sghtmltopdf.rb`. In the specs `spec_helper` loads `sghtmltopdf`
# first, so the same path is walked by hand
# (the guard itself is checked in a separate process by spec/railtie_spec.rb).
require "sghtmltopdf/railtie"

module Dummy
  class Application < ::Rails::Application
    config.load_defaults("#{::Rails::VERSION::MAJOR}.#{::Rails::VERSION::MINOR}")
    config.root = File.expand_path("dummy", __dir__)
    config.eager_load = false
    config.secret_key_base = "sghtmltopdf" * 8
    config.logger = Logger.new(IO::NULL)
    config.consider_all_requests_local = true
    # Exceptions are propagated to the test as-is (not wrapped in a 500 HTML page).
    config.action_dispatch.show_exceptions = :none
    config.hosts.clear
  end
end

Rails.application.initialize!

# The defaults the Railtie's initialiser injected. They are recorded straight after boot and
# the configuration is then reset, so the specs that do not use Rails are unaffected
# (spec order is random, and other specs empty the configuration with `reset_config!`).
CONFIG_AFTER_BOOT = Sghtmltopdf.config.to_h.freeze
Sghtmltopdf.reset_config!

module RailsAppHelpers
  include Rack::Test::Methods

  def app
    Rails.application
  end
end

RSpec.configure do |config|
  config.include RailsAppHelpers, type: :rails
  # Start each example from the same configuration as straight after boot.
  config.before(type: :rails) { Sghtmltopdf.config.apply_defaults(CONFIG_AFTER_BOOT) }
  config.after(type: :rails) { Sghtmltopdf.reset_config! }
end
