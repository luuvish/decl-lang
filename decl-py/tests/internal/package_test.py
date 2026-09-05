"""package (tests/internal/checks.json): the manifest reader and the
package hash the lock file rests on."""

from __future__ import annotations

import shutil
from pathlib import Path

from decl.package import package_hash, parse_manifest

ROOT = Path(__file__).resolve().parents[3]


def test_manifest(tmp_path: Path) -> None:
    def read(text: str) -> tuple:
        (tmp_path / "decl.toml").write_text(text, encoding="utf-8")
        codes: list[str] = []
        m = parse_manifest(str(tmp_path / "decl.toml"), lambda c, _m: codes.append(c))
        return m, codes

    good, codes = read('name = "app"\nversion = "1.0.0"\n\n[dependencies]\ncorelib = "1.0.0"\n')
    assert codes == [] and good is not None
    assert (good["name"], good["version"]) == ("app", "1.0.0")
    assert dict(good["dependencies"]) == {"corelib": "1.0.0"}
    _, codes = read(
        'name = "app"\nversion = "1.0.0"\nflavor = "x"\n\n[dependencies]\ncorelib = "^1.0"\n'
    )
    assert "E3011" in codes and "E3012" in codes


def test_hash(tmp_path: Path) -> None:
    corelib = ROOT / "tests/packages/app/decl_modules/corelib"
    locked = (ROOT / "tests/packages/lock/decl.lock").read_text(encoding="utf-8").split()[2]
    h1 = package_hash(str(corelib))
    assert h1 == locked
    assert package_hash(str(corelib)) == h1, "the same on a second call"
    copy = tmp_path / "corelib"
    shutil.copytree(corelib, copy)
    with open(copy / "types/base.decl", "a", encoding="utf-8") as f:
        f.write("// drift\n")
    assert package_hash(str(copy)) != h1, "different for a copy with one file appended to"
