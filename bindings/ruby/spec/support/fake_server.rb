# frozen_string_literal: true

require "socket"

# The minimal HTTP server standing in for `sghtmltopdf server`.
# It makes the status code assignment, the query string and chunked transfer handling
# testable deterministically without the real binary.
class FakeServer
  Request = Struct.new(:method, :path, :query, :body)

  attr_reader :requests, :port

  # A handler returns `[status, body]`. An array `body` selects chunked transfer.
  def initialize(handler = nil)
    @handler = handler || ->(_request) { [200, "%PDF-1.7 fake\n%%EOF"] }
    @requests = []
    @mutex = Mutex.new
    @socket = TCPServer.new("127.0.0.1", 0)
    @port = @socket.addr[1]
    @thread = Thread.new { accept_loop }
    @thread.abort_on_exception = false
  end

  def url
    "http://127.0.0.1:#{port}"
  end

  def last_request
    @mutex.synchronize { @requests.last }
  end

  def stop
    @thread&.kill
    @socket.close unless @socket.closed?
  end

  # Start the server and always stop it on leaving the block. The handler is an argument
  # (the block being the test itself).
  def self.run(handler = nil)
    server = new(handler)
    begin
      yield server
    ensure
      server.stop
    end
  end

  private

  def accept_loop
    loop do
      client = @socket.accept
      begin
        serve(client)
      rescue StandardError # rubocop:disable Lint/SuppressedException
        # A dropped connection is part of the test (a timeout and so on), so it is ignored.
      ensure
        client.close unless client.closed?
      end
    end
  end

  def serve(client)
    request = read_request(client)
    @mutex.synchronize { @requests << request }
    status, body = @handler.call(request)
    body.is_a?(Array) ? write_chunked(client, status, body) : write_plain(client, status, body.to_s)
  end

  def read_request(client)
    method, target, = client.gets.to_s.split(" ")
    headers = {}
    while (line = client.gets) && line.strip != ""
      key, value = line.split(":", 2)
      headers[key.to_s.strip.downcase] = value.to_s.strip
    end
    length = headers["content-length"].to_i
    body = length.positive? ? client.read(length) : ""
    path, query = target.to_s.split("?", 2)
    Request.new(method, path, query.to_s, body)
  end

  def write_plain(client, status, body)
    client.write(<<~HEAD.gsub("\n", "\r\n"))
      HTTP/1.1 #{status} #{status == 200 ? "OK" : "Error"}
      Content-Type: #{status == 200 ? "application/pdf" : "text/plain; charset=utf-8"}
      Content-Length: #{body.bytesize}
      Connection: close

    HEAD
    client.write(body)
  end

  def write_chunked(client, status, chunks)
    client.write(<<~HEAD.gsub("\n", "\r\n"))
      HTTP/1.1 #{status} OK
      Content-Type: application/pdf
      Transfer-Encoding: chunked
      Connection: close

    HEAD
    chunks.each do |chunk|
      client.write("#{chunk.bytesize.to_s(16)}\r\n#{chunk}\r\n")
      client.flush
    end
    client.write("0\r\n\r\n")
  end
end
