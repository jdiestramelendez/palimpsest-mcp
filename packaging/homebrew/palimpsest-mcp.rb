# Homebrew formula template for palimpsest-mcp.
#
# Lives in the tap repository jdiestramelendez/homebrew-tap as
# Formula/palimpsest-mcp.rb. After each release, replace VERSION and the
# two sha256 values with the ones from the release's SHA256SUMS file
# (see RELEASING.md at the project root).
class PalimpsestMcp < Formula
  desc "Persistent memory MCP server: hybrid retrieval over SQLite with time-travel"
  homepage "https://github.com/jdiestramelendez/palimpsest-mcp"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/jdiestramelendez/palimpsest-mcp/releases/download/v#{version}/palimpsest-mcp-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256"
    else
      url "https://github.com/jdiestramelendez/palimpsest-mcp/releases/download/v#{version}/palimpsest-mcp-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/jdiestramelendez/palimpsest-mcp/releases/download/v#{version}/palimpsest-mcp-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_LINUX_GNU_SHA256"
    else
      url "https://github.com/jdiestramelendez/palimpsest-mcp/releases/download/v#{version}/palimpsest-mcp-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_X86_64_LINUX_MUSL_SHA256"
    end
  end

  def install
    bin.install "palimpsest-mcp"
  end

  test do
    assert_match "palimpsest-mcp", shell_output("#{bin}/palimpsest-mcp --help")
  end
end
