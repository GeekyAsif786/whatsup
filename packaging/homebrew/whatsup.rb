# Template for GeekyAsif786/homebrew-tap: Formula/whatsup.rb.
# Don't edit the @...@ placeholders by hand; generate the real formula with
#   packaging/homebrew/fill-formula.sh <version> <path/to/SHA256SUMS>
# (see RELEASE.md).
class Whatsup < Formula
  desc "See what's listening on your dev machine: ports, servers, databases, containers"
  homepage "https://github.com/GeekyAsif786/whatsup"
  version "@VERSION@"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/GeekyAsif786/whatsup/releases/download/v@VERSION@/whatsup-v@VERSION@-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA256_AARCH64_APPLE_DARWIN@"
    end
    on_intel do
      url "https://github.com/GeekyAsif786/whatsup/releases/download/v@VERSION@/whatsup-v@VERSION@-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA256_X86_64_APPLE_DARWIN@"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/GeekyAsif786/whatsup/releases/download/v@VERSION@/whatsup-v@VERSION@-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA256_X86_64_UNKNOWN_LINUX_GNU@"
    end
  end

  def install
    bin.install "whatsup"
  end

  test do
    assert_equal "whatsup #{version}", shell_output("#{bin}/whatsup --version").strip
  end
end
