"""Install notices for both release executables from Cargo's actual build events."""

import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
from urllib.parse import quote


def output(*command):
    return subprocess.check_output(command, text=True).strip()


def collect_crate(package, dest):
    root = Path(package["manifest_path"]).parent
    # Keep upstream filenames and texts, including alternative licenses and NOTICEs.
    notices = [
        p for p in root.rglob("*") if p.is_file()
        and re.match(r"^(licen[cs]e|copying|copyright|notice|unlicense)([._-]|$)", p.name, re.I)
    ]
    if package["license_file"]:
        notices.append(root / package["license_file"])
    if not notices:
        raise ValueError(f"No upstream notices for {root.name}")
    for path in notices:
        target = dest / path.relative_to(root)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)

    # Cargo's license expression describes the crate as a whole. Individual
    # source files can carry extra conditions (e.g. futures-channel's MPSC queue).
    blocks = []
    comment = re.compile(r"/\*.*?\*/|^[ \t]*//[^\n]*(?:\n[ \t]*//[^\n]*)*", re.S | re.M)
    marker = re.compile(r"copyright\s+(?:\(c\)|©|\d{4})|SPDX-License-Identifier|redistribution and use in source", re.I)
    for path in sorted(root.rglob("*")):
        if path.suffix not in {".rs", ".c", ".h", ".cpp", ".S"}:
            continue
        for match in comment.finditer(path.read_text(encoding="utf-8")):
            if marker.search(match[0]):
                blocks.append(f"{path.relative_to(root)}\n{match[0]}\n")
    if blocks:
        (dest / "source-notices.txt").write_text("\n".join(blocks))


def rust_notices(dest):
    dest.mkdir()
    sysroot = Path(output("rustc", "--print", "sysroot"))
    # Debian/Ubuntu's package copyright includes notices for bundled components.
    libdir = Path(output("rustc", "--print", "target-libdir"))
    if shutil.which("dpkg-query"):
        library = next(libdir.glob("libstd-*.rlib"))
        result = subprocess.run(["dpkg-query", "-S", str(library)], capture_output=True, text=True)
        if result.returncode == 0:
            owner = result.stdout.split(":", 1)[0]
            shutil.copyfile(Path("/usr/share/doc") / owner / "copyright", dest / "copyright")
            package, version = output("dpkg-query", "-W", "-f=${source:Package} ${source:Version}", owner).split()
            if "ubuntu" in version:
                url = f"https://launchpad.net/ubuntu/+source/{package}/{quote(version)}"
            else:
                url = f"https://snapshot.debian.org/package/{package}/{quote(version)}/"
            return f"{package} {version}\nDistribution source: {url}"
    doc = sysroot / "share/doc/rust"
    if (doc / "COPYRIGHT.html").is_file():
        shutil.copyfile(doc / "COPYRIGHT.html", dest / "COPYRIGHT.html")
        for path in sorted(doc.glob("COPYRIGHT-library*")):
            shutil.copyfile(path, dest / path.name)
        shutil.copytree(doc / "licenses", dest / "licenses")
        return "Upstream Rust: https://github.com/rust-lang/rust (commit-hash below)"
    if sysroot == Path("/usr") and Path("/usr/share/licenses/rust/COPYRIGHT-library.html.rustc").is_file():
        shutil.copyfile("/usr/share/licenses/rust/COPYRIGHT-library.html.rustc", dest / "COPYRIGHT-library.html")
        shutil.copytree("/usr/share/doc/rustc/licenses", dest / "licenses")
        return (output("pacman", "-Q", "rust")
                + "\nDistribution recipes: https://gitlab.archlinux.org/archlinux/packaging/packages/rust")
    raise ValueError("No standard-library notices found for the active rustc")


def debian_copyright(dest):
    # Debian Policy 12.5: keep an uncompressed copyright file. List files
    # with identical upstream texts together, preserving the texts verbatim.
    texts = {}
    third = dest / "third-party"
    for p in sorted(third.rglob("*")):
        if p.is_file() and p.name != "INDEX":
            texts.setdefault(p.read_bytes(), []).append(str(p.relative_to(third)))
    with (dest / "copyright").open("wb") as f:
        f.write(b"Veyra\nSource: https://github.com/68RTF/veyra\n"
                b"Copyright (C) 2026 68RTF\nLicense: GPL-3.0-only\n"
                b"Full text: /usr/share/common-licenses/GPL-3 and /usr/share/doc/veyra/LICENSE\n"
                b"Corresponding source archive and digest: /usr/share/doc/veyra/SOURCE\n\n")
        for data, paths in texts.items():
            f.write(("\n".join(paths) + "\n\n").encode())
            f.write(data + b"\n\n")


def main():
    events, dest, digest, distro = sys.argv[1:]
    if not re.fullmatch("[a-f0-9]{64}", digest):
        raise ValueError("Missing corresponding-source digest")
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    shutil.copyfile("LICENSE", dest / "LICENSE")
    metadata = json.loads(output("cargo", "metadata", "--format-version", "1", "--frozen"))
    root = metadata["resolve"]["root"]
    packages = {p["id"]: p for p in metadata["packages"]}
    crates = set()
    bins = set()
    for line in Path(events).read_text().splitlines():
        event = json.loads(line)
        if event["reason"] == "compiler-artifact":
            if event["package_id"] != root:
                crates.add(event["package_id"])
            elif event["executable"]:
                bins.add(event["target"]["name"])
    if bins != {"veyra", "ym-export"} or not crates:
        raise ValueError("Incomplete Cargo build record")
    third = dest / "third-party"
    rows = ["Rust crates compiled for the two binaries, including build-time tools:"]
    for key in sorted(crates):
        p = packages[key]
        name = f'{p["name"]}-{p["version"]}'
        collect_crate(p, third / name)
        rows.append(f'{name}: {p["license"] or p["license_file"]}')
    (third / "INDEX").write_text("\n".join(rows) + "\n")
    rust_source = rust_notices(third / "rust-stdlib")
    if distro == "deb":
        debian_copyright(dest)
    version = packages[root]["version"]
    (dest / "SOURCE").write_text(
        "Veyra — Copyright (C) 2026 68RTF\nLicense: GPL-3.0-only\n\n"
        f"Release source: https://github.com/68RTF/veyra/releases/download/v{version}/veyra-{version}-source.tar.gz\n"
        f"Archive SHA-256: {digest}\n"
        "Extract it and run cargo build --release --frozen --bins in its top directory.\n"
        "Build prerequisites and user installation: README.md.\n"
        "Distribution recipes: packaging/arch/PKGBUILD and scripts/build-*.sh.\n\n"
        + "Rust standard library: " + rust_source + "\n" + output("rustc", "-vV") + "\n" + output("cargo", "-V") + "\n"
    )


if __name__ == "__main__":
    main()
