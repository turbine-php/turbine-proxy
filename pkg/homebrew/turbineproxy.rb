# Homebrew formula for TurbineProxy
# https://brew.sh
#
# To install from source (local tap):
#   brew install --formula ./pkg/homebrew/turbineproxy.rb
#
# Usage after install:
#   turbineproxy --help

class Turbineproxy < Formula
  desc "Intelligent MySQL proxy with read/write splitting, analytics, and Prometheus metrics"
  homepage "https://github.com/your-org/turbineproxy"
  license "MIT"

  # ── Update SHA and URL on each release ──────────────────────────────────────
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/your-org/turbineproxy/releases/download/v#{version}/turbineproxy-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_SHA256_AARCH64_DARWIN"
    else
      url "https://github.com/your-org/turbineproxy/releases/download/v#{version}/turbineproxy-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_SHA256_X86_64_DARWIN"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/your-org/turbineproxy/releases/download/v#{version}/turbineproxy-aarch64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_SHA256_AARCH64_LINUX"
    else
      url "https://github.com/your-org/turbineproxy/releases/download/v#{version}/turbineproxy-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_SHA256_X86_64_LINUX"
    end
  end

  def install
    bin.install "turbineproxy"
    pkgshare.install "dashboard" if Dir.exist?("dashboard")
    etc.install "turbineproxy.example.toml" => "turbineproxy.example.toml"
  end

  def post_install
    unless (etc/"turbineproxy.toml").exist?
      cp etc/"turbineproxy.example.toml", etc/"turbineproxy.toml"
    end
  end

  service do
    run [opt_bin/"turbineproxy"]
    working_dir pkgshare
    log_path var/"log/turbineproxy.log"
    error_log_path var/"log/turbineproxy.log"
    keep_alive true
    process_type :background
  end

  def caveats
    <<~EOS
      Configuration file: #{etc}/turbineproxy.toml
      Edit it to point at your MySQL server, then start the service.

      To start TurbineProxy now and at login:
        brew services start turbineproxy

      MySQL proxy port:  3307 (clients connect here)
      Dashboard port:    8080 (http://localhost:8080)
      Prometheus metrics: http://localhost:8080/metrics
    EOS
  end

  test do
    assert_match "turbineproxy", shell_output("#{bin}/turbineproxy --help 2>&1", 1)
  end
end
