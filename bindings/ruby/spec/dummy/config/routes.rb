# frozen_string_literal: true

Rails.application.routes.draw do
  %w[show download with_layout as_html with_stylesheet with_image receipt bad_option].each do |action|
    get "/invoices/#{action}", to: "invoices##{action}"
  end
  get "/streams/show", to: "streams#show"
end
