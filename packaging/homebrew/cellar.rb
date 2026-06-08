# Homebrew formula for the `cellar` CLI — CEL, the trust + execution layer
# for AI-operated computers (WS12).
#
# This is a BUILD-FROM-SOURCE formula: it compiles `cellar-cli` from the
# published GitHub release *source* tarball (which GitHub generates for every
# tag), so NO separate prebuilt-binary release artifact is required. Ship it
# via a tap:
#
#   brew tap dilipod/cellar https://github.com/dimpagk92/cellar
#   brew install cellar
#
# MAINTAINER — before publishing a release:
#   1. set `url` to the real tag tarball (bump the version), and
#   2. replace the `sha256` placeholder with the real digest:
#        curl -sL <url> | shasum -a 256
#   3. copy this file into the tap repo under `Formula/cellar.rb`.
# See docs/distribution.md for the full release flow (and the future
# prebuilt-bottle path).
class Cellar < Formula
  desc "Trust and execution layer for AI-operated computers (CEL CLI)"
  homepage "https://github.com/dimpagk92/cellar"
  url "https://github.com/dimpagk92/cellar/archive/refs/tags/v0.1.0.tar.gz"
  # TODO(maintainer): replace with the real release tarball digest.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "Apache-2.0"
  head "https://github.com/dimpagk92/cellar.git", branch: "main"

  depends_on "rust" => :build

  def install
    # `cellar-cli` is a workspace member exposing the `cellar` binary.
    # `--locked` builds against the committed Cargo.lock for reproducibility.
    system "cargo", "install", "--locked", "--path", "cellar-cli", "--root", prefix
  end

  test do
    # `--help` and `completions` are no-daemon subcommands (the latter from
    # WS13), so both run in Homebrew's sandbox without a running cellar daemon.
    assert_match "cellar", shell_output("#{bin}/cellar --help")
    assert_match "compdef", shell_output("#{bin}/cellar completions zsh")
  end
end
