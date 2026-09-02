# Homebrew formula for the `decl` CLI (tap: luuvish/homebrew-tap, installed as `luuvish/tap/decl-lang`).
# Installs the published npm package `decl-lang` under libexec and links
# the `decl` and `decl-lsp` binaries — the standard Homebrew pattern for
# Node-based command-line tools.
#
# Release procedure (see ../../README.md): publish decl-lang to npm, then
# confirm `sha256` against the registry tarball — `npm pack` is
# reproducible, but verify with `brew fetch --build-from-source
# ./Formula/decl-lang.rb` before pushing the tap.
class DeclLang < Formula
  desc "Declarative language for describing, generating, and validating structured data"
  homepage "https://github.com/luuvish/decl-lang"
  url "https://registry.npmjs.org/decl-lang/-/decl-lang-0.2.0.tgz"
  sha256 "c01430b95bfaa85ebb1f48c0d0dbf61abc8d3721a83d10f66da3567676d1acff"
  license "MIT"

  depends_on "node"

  def install
    system "npm", "install", *std_npm_args
    bin.install_symlink Dir["#{libexec}/bin/*"]
  end

  test do
    (testpath/"t.decl").write <<~EOS
      type T = { a: int, const b = a * 2 }
      export output t: T = { a: 21 }
    EOS
    assert_equal "{\"a\":21,\"b\":42}", shell_output("#{bin}/decl evaluate #{testpath}/t.decl --root t").strip

    (testpath/"bad.decl").write "type Bad = 10..3\n"
    assert_match "E4011", shell_output("#{bin}/decl check #{testpath}/bad.decl 2>&1", 1)
  end
end
