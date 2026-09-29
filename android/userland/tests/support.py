import io
import logging
import os
import stat
import struct
import sys
import zipfile
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import package

logging.getLogger("package").addHandler(logging.NullHandler())
logging.getLogger("package").propagate = False

PT_LOAD = 1
ET_REL = 1
EM_X86_64 = 62
EM_AARCH64 = 183
DEVICE_PREFIX = "/data/data/io.github.blendonl.amux/files/usr"
INTERPRETER = b"/system/bin/linker64\0"


def elf(
    elf_type: int = package.ET_DYN,
    interp: bool = True,
    entry: int = 0x1000,
    elf_class: int = 2,
    order: str = "<",
    machine: int = EM_X86_64,
    tag: bytes = b"",
) -> bytes:
    header_format = order + package.ELF_HEADER_FORMATS[elf_class]
    header_size = 16 + struct.calcsize(header_format)
    entry_size = 56 if elf_class == 2 else 32
    types = [PT_LOAD, package.PT_INTERP] if interp else [PT_LOAD]
    ident = package.ELF_MAGIC + bytes([elf_class, 1 if order == "<" else 2, 1]) + bytes(9)
    header = struct.pack(
        header_format, elf_type, machine, 1, entry, header_size, 0, 0, header_size, entry_size, len(types), 0, 0, 0
    )
    table = b"".join(struct.pack(order + "I", p_type).ljust(entry_size, b"\0") for p_type in types)
    return ident + header + table + INTERPRETER + tag


@dataclass(frozen=True)
class Regular:
    path: str
    data: bytes
    mode: int = 0o644

    def create(self, root: Path) -> None:
        target = root / self.path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(self.data)
        target.chmod(self.mode)


@dataclass(frozen=True)
class Symlink:
    path: str
    target: str

    def create(self, root: Path) -> None:
        link = root / self.path
        link.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(self.target, link)


@dataclass(frozen=True)
class Hardlink:
    path: str
    existing: str

    def create(self, root: Path) -> None:
        link = root / self.path
        link.parent.mkdir(parents=True, exist_ok=True)
        os.link(root / self.existing, link)


@dataclass(frozen=True)
class Directory:
    path: str

    def create(self, root: Path) -> None:
        (root / self.path).mkdir(parents=True, exist_ok=True)


Node = Regular | Symlink | Hardlink | Directory


def sample_nodes() -> list[Node]:
    return [
        Regular("bin/zsh", elf(tag=b"zsh"), 0o755),
        Hardlink("bin/zsh-5.9", "bin/zsh"),
        Regular("bin/git", elf(tag=b"git"), 0o755),
        Regular("libexec/git-core/git", elf(tag=b"git"), 0o755),
        Regular("libexec/git-core/git-remote-http", elf(tag=b"remote-http"), 0o755),
        Regular("libexec/a/b/c/tool", elf(tag=b"tool"), 0o755),
        Regular("libexec/only-exec/runner", elf(tag=b"runner"), 0o755),
        Regular("bin/ls", elf(package.ET_EXEC, interp=False, tag=b"ls"), 0o755),
        Regular("bin/toybox", elf(interp=False, entry=0x2000, tag=b"toybox"), 0o755),
        Regular("bin/no-exec-bit", elf(interp=False, entry=0x2000, tag=b"no-exec-bit"), 0o644),
        Regular("bin/c++", elf(tag=b"c++"), 0o755),
        Regular("bin/c__", elf(tag=b"c__"), 0o755),
        Regular("bin/my tool@2", elf(tag=b"my tool"), 0o755),
        Regular("bin/nano", elf(tag=b"nano"), 0o755),
        Symlink("bin/vi", "nano"),
        Symlink("bin/view", f"{DEVICE_PREFIX}/bin/nano"),
        Symlink("bin/sh", "/system/bin/sh"),
        Regular("bin/zcat", b'#!/bin/sh\nexec gzip -cd "$@"\n', 0o755),
        Regular("lib/libz.so.1.3", elf(interp=False, entry=0, tag=b"libz"), 0o644),
        Symlink("lib/libz.so", "libz.so.1.3"),
        Regular("lib/libc++_shared.so", elf(interp=False, entry=0x1000, tag=b"libc++"), 0o755),
        Regular("lib/zsh/5.9/zsh/zle.so", elf(interp=False, entry=0, tag=b"zle"), 0o755),
        Regular("share/doc/zsh/README", b"zsh docs\n", 0o644),
        Regular("share/broken.elf", package.ELF_MAGIC + b"\x02\x01\x01" + bytes(20), 0o644),
        Regular("share/objects/start.o", elf(ET_REL, interp=False, entry=0), 0o644),
        Regular("etc/profile", b"export PATH\n", 0o644),
        Regular("libexec/suid-helper", b"#!/bin/sh\necho hi\n", 0o4755),
        Directory("var/empty"),
        Directory("tmp"),
        Symlink("var/cache/links/current", "../../../etc/profile"),
    ]


def build(root: Path, nodes: Iterable[Node]) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    for node in nodes:
        node.create(root)
    return root


def reversed_build_order(nodes: list[Node]) -> list[Node]:
    first = [node for node in reversed(nodes) if not isinstance(node, Hardlink)]
    return first + [node for node in nodes if isinstance(node, Hardlink)]


def outputs(root: Path) -> dict[str, tuple[int, bytes]]:
    return {
        path: (stat.S_IMODE(status.st_mode), (root / path).read_bytes())
        for path, status in package.walk(root)
        if stat.S_ISREG(status.st_mode)
    }


def member_kind(archive: zipfile.ZipFile, info: zipfile.ZipInfo) -> package.Kind:
    try:
        elf_header = package.read_elf(io.BytesIO(archive.read(info)))
    except package.MalformedElf:
        elf_header = None
    return package.classify(info.filename, info.external_attr >> 16, elf_header)


def executables_in_archive(archive_path: Path) -> list[str]:
    with zipfile.ZipFile(archive_path) as archive:
        return [
            info.filename
            for info in archive.infolist()
            if not info.is_dir() and member_kind(archive, info).is_executable
        ]


def path_problems(path: str, status: os.stat_result, before: Path, after: Path) -> list[str]:
    if stat.S_ISLNK(status.st_mode) and (not after.is_symlink() or os.readlink(after) != os.readlink(before)):
        return [f"{path}: symlink target changed"]
    if (before.is_dir(), before.is_file()) != (after.is_dir(), after.is_file()):
        return [f"{path}: resolves to a different kind of file"]
    if before.is_file() and before.read_bytes() != after.read_bytes():
        return [f"{path}: content differs"]
    return []


def round_trip_problems(original: Path, installed: Path) -> list[str]:
    problems = []
    seen = set()
    for path, status in package.walk(original):
        seen.add(path)
        problems += path_problems(path, status, original / path, installed / path)
    extra = [path for path, _ in package.walk(installed) if path not in seen and path != package.VERSION]
    return problems + [f"{path}: not in the original prefix" for path in extra]
