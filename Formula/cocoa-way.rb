class CocoaWay < Formula
  desc "Native macOS Wayland compositor for running Linux apps"
  homepage "https://github.com/J-x-Z/cocoa-way"
  url "https://github.com/J-x-Z/cocoa-way.git", tag: "v1.0.0"
  sha256 "3f79550d2f5cd4e0e40db983236843d19818127a0e5eaba56baa6519afdab722"
  license "GPL-3.0-only"
  head "https://github.com/J-x-Z/cocoa-way.git", branch: "main"

  depends_on "rust" => :build
  depends_on "pkg-config" => :build
  depends_on "libxkbcommon"
  depends_on "pixman"
  depends_on :macos

  def install
    system "cargo", "build", "--release"
    bin.install "target/release/cocoa-way"
  end

  def caveats
    <<~EOS
      Cocoa-Way is a Wayland compositor for running Linux GUI apps on macOS.
      
      Quick start:
        1. Start the compositor:
           cocoa-way

        2. Connect Linux clients via waypipe:
           brew install J-x-Z/tap/waypipe-darwin
           waypipe ssh user@linux-host <program>

      For more info: https://github.com/J-x-Z/cocoa-way
    EOS
  end

  test do
    # Basic smoke test - check binary runs
    assert_match "cocoa-way", shell_output("#{bin}/cocoa-way --help 2>&1", 2)
  end
end
