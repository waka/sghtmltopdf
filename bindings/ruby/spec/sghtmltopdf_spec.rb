# frozen_string_literal: true

RSpec.describe Sghtmltopdf do
  it "has a version" do
    expect(Sghtmltopdf::VERSION).to match(/\A\d+\.\d+\.\d+\z/)
  end

  describe "the native extension" do
    it "links against the core" do
      # The result of calling the core's PageSettings::default() (A4's size in px).
      expect(Sghtmltopdf::Native.default_page_size).to eq("793.7x1122.5")
    end

    it "releases the GVL so other threads make progress concurrently" do
      # Four threads x 300ms. Holding the GVL throughout would take about 1200ms.
      elapsed = elapsed_seconds do
        4.times.map { Thread.new { Sghtmltopdf::Native.sleep_without_gvl(300) } }.each(&:join)
      end
      expect(elapsed).to be < 0.9
    end
  end
end
