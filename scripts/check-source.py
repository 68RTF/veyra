#!/usr/bin/env python3
"""Reject local working documents and unexpected project files in release source."""
from pathlib import PurePosixPath
import sys
import tarfile
import unittest
import io
import tempfile


ROOT_FILES = {"Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "veyra.desktop"}
DOC_ASSETS = {"player.png", "veyra-logo.svg", "demo-cover.svg"}


def public_file(name):
    path = PurePosixPath(name)
    parts = path.parts
    if path.is_absolute() or ".." in parts or not parts:
        return False
    if parts[0] in {"vendor", "source-notices"}:
        return True  # Preserve upstream source and mandatory notices unchanged.
    if name in ROOT_FILES or name == ".cargo/config.toml":
        return True
    if path.suffix.lower() == ".md":
        return False
    if len(parts) == 2 and parts[0] == "docs":
        return parts[1] in DOC_ASSETS
    if parts[0] == "src":
        return path.suffix in {".rs", ".css"}
    if parts[0] == "assets":
        return path.suffix in {".svg", ".png"}
    if len(parts) == 3 and parts[:2] == ("packaging", "arch"):
        return parts[2] == "PKGBUILD"
    if len(parts) == 3 and parts[:2] == (".github", "workflows"):
        return path.suffix in {".yml", ".yaml"}
    if len(parts) == 2 and parts[0] == "scripts":
        return path.suffix in {".py", ".sh"}
    return False


def check_archive(filename):
    with tarfile.open(filename, "r:gz") as archive:
        names = set()
        roots = set()
        for member in archive:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or not path.parts:
                raise ValueError("Unsafe archive path")
            roots.add(path.parts[0])
            if member.isdir():
                continue
            relative = "/".join(path.parts[1:])
            if not public_file(relative):
                raise ValueError(f"Unexpected project file: {relative}")
            names.add(relative)
        if len(roots) != 1 or not ROOT_FILES <= names:
            raise ValueError("Incomplete source archive or multiple root directories")


class SourcePolicyTests(unittest.TestCase):
    def archive(self, extra=(), missing=()):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            for name in (ROOT_FILES - set(missing)) | set(extra):
                member = tarfile.TarInfo("veyra-test/" + name)
                member.size = 1
                archive.addfile(member, io.BytesIO(b"x"))
        return data.getvalue()

    def check(self, data):
        with tempfile.NamedTemporaryFile(suffix=".tar.gz") as file:
            file.write(data)
            file.flush()
            check_archive(file.name)

    def test_complete_archive(self):
        self.check(self.archive({"src/main.rs", "docs/player.png", "vendor/crate/README.md"}))

    def test_archive_rejects_internal_document(self):
        for name in ["REVIEW.md", "docs/architecture.md", "AGENTS.md", "notes.txt"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.check(self.archive({name}))

    def test_archive_requires_license_and_manifest(self):
        for name in ["LICENSE", "Cargo.toml"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.check(self.archive(missing={name}))

    def test_truncated_archive_is_rejected(self):
        with self.assertRaises((tarfile.TarError, EOFError)):
            self.check(b"not an archive")

    def test_required_sources(self):
        for path in ROOT_FILES | {"src/main.rs", "src/mpv/tests.rs", "src/style.css"}:
            with self.subTest(path=path):
                self.assertTrue(public_file(path))

    def test_internal_documents(self):
        for path in {"AGENTS.md", "WRITING_STYLE.md", "REVIEW.md", "ROADMAP.md",
                     "docs/architecture.md", "docs/development.md", "docs/release-notes.md",
                     "src/AGENTS.md", "scripts/report.md"}:
            with self.subTest(path=path):
                self.assertFalse(public_file(path))

    def test_local_data_and_build_outputs(self):
        for path in {"playlist.csv", "session.json", ".env", ".git/config",
                     "backups/state.tar.gz", "artifacts/player.png", "target/veyra",
                     "docs/notes.txt", "scripts/token.json"}:
            with self.subTest(path=path):
                self.assertFalse(public_file(path))

    def test_public_assets(self):
        for name in DOC_ASSETS:
            self.assertTrue(public_file("docs/" + name))

    def test_upstream_notices(self):
        for path in {"vendor/crate/LICENSE", "vendor/crate/README.md",
                     "source-notices/crate/LICENSE-MIT"}:
            self.assertTrue(public_file(path))

    def test_unsafe_paths(self):
        for path in {"/src/main.rs", "../README.md", "src/../../AGENTS.md",
                     "vendor/crate/../../../AGENTS.md"}:
            self.assertFalse(public_file(path))

    def test_build_inputs(self):
        for path in {".cargo/config.toml", ".github/workflows/build.yml",
                     "packaging/arch/PKGBUILD", "scripts/build-source.sh"}:
            self.assertTrue(public_file(path))


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        unittest.main(argv=[sys.argv[0]])
    elif len(sys.argv) == 2:
        try:
            check_archive(sys.argv[1])
        except (ValueError, tarfile.TarError, OSError) as error:
            sys.exit(str(error))
    else:
        sys.exit("Usage: check-source.py ARCHIVE | --self-test")
