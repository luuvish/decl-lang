# Homebrew formula for the `decl` CLI (tap: luuvish/homebrew-tap, installed as `luuvish/tap/decl-lang`).
# Installs the published npm package `decl-lang` under libexec and links
# the `decl` and `decl-lsp` binaries — the standard Homebrew pattern for
# Node-based command-line tools.
#
# Release procedure (see ../../README.md): the `homebrew` job of
# .github/workflows/release.yml renders `url` and `sha256` from the npm
# registry after each publication and pushes this file to the tap; the
# copy here is kept the same by that job.
class DeclLang < Formula
  desc "Declarative language for describing, generating, and validating structured data"
  homepage "https://decl-lang.org/"
  url "https://registry.npmjs.org/decl-lang/-/decl-lang-0.4.0.tgz"
  sha256 "69872242085a0cc47d06a66188e183c5707de6be5911deff5b83c49e9cfdda8a"
  license "MIT"

  depends_on "node"

  def install
    system "npm", "install", *std_npm_args
    bin.install_symlink Dir["#{libexec}/bin/*"]
  end

  test do
    (testpath/"t.decl").write <<~EOS
      type T = { a: int, b = a * 2 }
      export output t: T = { a: 21 }
    EOS
    assert_equal "{\"a\":21,\"b\":42}", shell_output("#{bin}/decl evaluate #{testpath}/t.decl --output t").strip

    (testpath/"bad.decl").write "type Bad = 10..3\n"
    assert_match "E4011", shell_output("#{bin}/decl check #{testpath}/bad.decl 2>&1", 1)
  end
end
