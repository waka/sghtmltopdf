# frozen_string_literal: true

# Start the dummy app on a web server for manual checking.
#
#   bundle exec rackup spec/dummy/config.ru
#   → http://localhost:9292/invoices/show
#
# The same app as the specs' `spec/rails_helper.rb`, but that one calls `reset_config!`
# straight after boot (for spec independence). Here the defaults the Railtie injected are to
# be kept, so it is assembled separately.
ENV["RAILS_ENV"] ||= "development"

require "bundler/setup"
require "logger"
require "rails"
require "action_controller/railtie"
require "sghtmltopdf"
require "sghtmltopdf/railtie"

module DummyServer
  class Application < ::Rails::Application
    config.load_defaults("#{::Rails::VERSION::MAJOR}.#{::Rails::VERSION::MINOR}")
    config.root = __dir__
    config.eager_load = false
    config.secret_key_base = "sghtmltopdf" * 8
    config.logger = Logger.new($stdout)
    # Exceptions print a backtrace to the browser.
    config.consider_all_requests_local = true
    config.hosts.clear
  end
end

Rails.application.initialize!

run Rails.application
