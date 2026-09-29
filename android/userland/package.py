#!/usr/bin/env python3
import argparse
import enum
import hashlib
import logging
import os
import re
import shutil
import stat
import struct
import sys
import tempfile
import zipfile
from collections import defaultdict
from collections.abc import Iterable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import BinaryIO

log = logging.getLogger("package")

ABI_MACHINES = {"arm64-v8a": 183, "x86_64": 62}
LIB_PREFIX = "libu_"
LIB_SUFFIX = ".so"
MAX_LIB_NAME = 255
NAME_HASH_LENGTH = 8
UNSAFE_NAME_CHARS = re.compile(r"[^A-Za-z0-9._-]")
APPLIB = "applib"
SYMLINKS = "SYMLINKS.txt"
VERSION = "USERLAND_VERSION"
RESERVED_NAMES = frozenset({SYMLINKS, VERSION})
LINK_ARROW = "←"
UNREPRESENTABLE = ("\n", "\r", LINK_ARROW)
EXEC_DIRS = frozenset({"bin", "libexec"})
VERSION_FORMAT = b"amux-userland 1\n"

ZIP_TIME = (1980, 1, 1, 0, 0, 0)
ZIP_LEVEL = 9
ZIP_UNIX = 3
ZIP_DOS_DIRECTORY = 0x10
LIB_MODE = 0o755
ARCHIVE_MODE = 0o644
DIR_MODE = 0o700
EXEC_BITS = 0o111
SPECIAL_BITS = 0o7000

ELF_MAGIC = b"\x7fELF"
ELF_HEADER_FORMATS = {1: "HHIIIIIHHHHHH", 2: "HHIQQQIHHHHHH"}
ELF_BYTE_ORDERS = {1: "<", 2: ">"}
ET_EXEC = 2
ET_DYN = 3
PT_INTERP = 3


class PackageError(Exception):
    pass


class MalformedElf(Exception):
    pass


@dataclass(frozen=True)
class Elf:
    type: int
    machine: int
    entry: int
    interp: bool


class Kind(enum.Enum):
    DATA = "data"
    LIBRARY = "shared library"
    EXECUTABLE = "executable"
    STATIC_PIE = "static-pie"

    @property
    def is_executable(self) -> bool:
        return self in (Kind.EXECUTABLE, Kind.STATIC_PIE)


@dataclass(frozen=True)
class File:
    path: str
    source: Path
    mode: int
    kind: Kind
    digest: str
    elf: Elf | None


@dataclass(frozen=True)
class Scan:
    files: tuple[File, ...]
    symlinks: dict[str, str]
    dirs: tuple[str, ...]


@dataclass(frozen=True)
class Library:
    name: str
    source: Path
    digest: str
    paths: tuple[str, ...]


@dataclass(frozen=True)
class Member:
    name: str
    mode: int
    source: Path | None = None
    data: bytes = b""

    def read(self) -> bytes:
        return self.source.read_bytes() if self.source else self.data


@dataclass(frozen=True)
class Summary:
    abi: str
    files: int
    executables: int
    libraries: int
    symlinks: int
    original_symlinks: int
    bare_dirs: int
    version: str


def read_elf(stream: BinaryIO) -> Elf | None:
    ident = stream.read(16)
    if len(ident) < 16 or not ident.startswith(ELF_MAGIC):
        return None
    header_format = ELF_HEADER_FORMATS.get(ident[4])
    order = ELF_BYTE_ORDERS.get(ident[5])
    if header_format is None or order is None:
        raise MalformedElf(f"unknown ELF class {ident[4]} or byte order {ident[5]}")
    header = struct.Struct(order + header_format)
    raw = stream.read(header.size)
    if len(raw) < header.size:
        raise MalformedElf("truncated ELF header")
    elf_type, machine, _, entry, phoff, _, _, _, phentsize, phnum, *_ = header.unpack(raw)
    return Elf(elf_type, machine, entry, has_interp(stream, order, phoff, phentsize, phnum))


def has_interp(stream: BinaryIO, order: str, phoff: int, phentsize: int, phnum: int) -> bool:
    if phnum == 0:
        return False
    if phentsize < 4:
        raise MalformedElf(f"program header entries of {phentsize} bytes")
    stream.seek(phoff)
    table = stream.read(phentsize * phnum)
    if len(table) < phentsize * phnum:
        raise MalformedElf("truncated program headers")
    p_type = struct.Struct(order + "I")
    return any(p_type.unpack_from(table, offset)[0] == PT_INTERP for offset in range(0, len(table), phentsize))


def classify(path: str, mode: int, elf: Elf | None) -> Kind:
    if elf is None:
        return Kind.DATA
    if elf.type == ET_EXEC or (elf.type == ET_DYN and elf.interp):
        return Kind.EXECUTABLE
    if elf.type != ET_DYN:
        return Kind.DATA
    parts = PurePosixPath(path).parts
    if len(parts) > 1 and parts[0] in EXEC_DIRS and mode & EXEC_BITS and elf.entry:
        return Kind.STATIC_PIE
    return Kind.LIBRARY


def inspect_elf(path: str, source: Path) -> Elf | None:
    with source.open("rb") as stream:
        try:
            return read_elf(stream)
        except MalformedElf as error:
            log.warning("%s: %s, keeping it as data", path, error)
            return None


def walk(root: Path, directory: str = "") -> Iterator[tuple[str, os.stat_result]]:
    for name in sorted(os.listdir(root / directory)):
        path = f"{directory}/{name}" if directory else name
        status = os.lstat(root / path)
        yield path, status
        if stat.S_ISDIR(status.st_mode):
            yield from walk(root, path)


def check_representable(path: str, what: str, text: str) -> None:
    if any(char in text for char in UNREPRESENTABLE):
        raise PackageError(f"{path}: {what} {text!r} can't be written to {SYMLINKS}")


def check_path(path: str) -> None:
    check_representable(path, "path", path)
    if path in RESERVED_NAMES:
        raise PackageError(f"{path}: the name is reserved for the packager")


def file_digest(source: Path) -> str:
    with source.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def scan(prefix: Path) -> Scan:
    files: list[File] = []
    symlinks: dict[str, str] = {}
    dirs: list[str] = []
    digests: dict[tuple[int, int], str] = {}
    for path, status in walk(prefix):
        check_path(path)
        source = prefix / path
        if stat.S_ISLNK(status.st_mode):
            target = os.readlink(source)
            check_representable(path, "symlink target", target)
            symlinks[path] = target
        elif stat.S_ISDIR(status.st_mode):
            dirs.append(path)
        elif stat.S_ISREG(status.st_mode):
            files.append(scan_file(path, source, status, digests))
        else:
            log.warning("%s: skipping a special file", path)
    return Scan(tuple(files), symlinks, tuple(dirs))


def scan_file(path: str, source: Path, status: os.stat_result, digests: dict[tuple[int, int], str]) -> File:
    inode = (status.st_dev, status.st_ino)
    if inode not in digests:
        digests[inode] = file_digest(source)
    elf = inspect_elf(path, source)
    kind = classify(path, status.st_mode, elf)
    if kind is Kind.STATIC_PIE:
        log.info("%s: an executable ELF without an interpreter; shipping it as a static-pie", path)
    return File(path, source, stat.S_IMODE(status.st_mode), kind, digests[inode], elf)


def check_machines(files: Iterable[File], abi: str) -> None:
    expected = ABI_MACHINES[abi]
    for file in files:
        if file.kind.is_executable and file.elf and file.elf.machine != expected:
            log.warning("%s: built for ELF machine %d, not %s (%d)", file.path, file.elf.machine, abi, expected)


def sanitize(path: str) -> str:
    return UNSAFE_NAME_CHARS.sub("_", path)


def library_name(stem: str, path: str, unique: bool) -> str:
    plain = f"{LIB_PREFIX}{stem}{LIB_SUFFIX}"
    if unique and len(plain) <= MAX_LIB_NAME:
        return plain
    tag = hashlib.sha256(encode(path)).hexdigest()[:NAME_HASH_LENGTH]
    room = MAX_LIB_NAME - len(LIB_PREFIX) - len(LIB_SUFFIX) - len(tag) - 1
    return f"{LIB_PREFIX}{stem[:room]}_{tag}{LIB_SUFFIX}"


def library_names(canonical_paths: Iterable[str]) -> dict[str, str]:
    by_stem: dict[str, list[str]] = defaultdict(list)
    for path in canonical_paths:
        by_stem[sanitize(path)].append(path)
    names = {
        path: library_name(stem, path, unique=len(paths) == 1)
        for stem, paths in by_stem.items()
        for path in paths
    }
    if len(set(names.values())) < len(names):
        raise PackageError("library names still collide after hashing")
    return names


def group_executables(files: Iterable[File]) -> list[Library]:
    groups: dict[str, list[File]] = defaultdict(list)
    for file in files:
        if file.kind.is_executable:
            groups[file.digest].append(file)
    members = [sorted(group, key=lambda file: file.path) for group in groups.values()]
    names = library_names(group[0].path for group in members)
    libraries = (
        Library(names[group[0].path], group[0].source, group[0].digest, tuple(file.path for file in group))
        for group in members
    )
    return sorted(libraries, key=lambda library: library.name)


def applib_target(path: str, name: str) -> str:
    return "../" * len(PurePosixPath(path).parts) + f"{APPLIB}/{name}"


def all_symlinks(original: dict[str, str], libraries: Iterable[Library]) -> dict[str, str]:
    links = dict(original)
    for library in libraries:
        links.update((path, applib_target(path, library.name)) for path in library.paths)
    return links


def render_symlinks(links: dict[str, str]) -> bytes:
    lines = (f"{target}{LINK_ARROW}./{path}\n" for path, target in sorted(links.items()))
    return "".join(lines).encode("utf-8", "surrogateescape")


def bare_dirs(dirs: Iterable[str], file_paths: Iterable[str]) -> list[str]:
    dirs = list(dirs)
    covered = {str(parent) for path in [*file_paths, *dirs] for parent in PurePosixPath(path).parents}
    return [directory for directory in dirs if directory not in covered]


def archive_mode(mode: int) -> int:
    return 0o700 if mode & EXEC_BITS else 0o600


def warn_special_bits(files: Iterable[File]) -> None:
    for file in files:
        if file.mode & SPECIAL_BITS:
            log.warning("%s: dropping setuid, setgid or sticky bits from mode %o", file.path, file.mode)


def encode(text: str) -> bytes:
    return text.encode("utf-8", "surrogateescape")


def userland_version(data: Iterable[File], dirs: Iterable[str], libraries: Iterable[Library], symlinks: bytes) -> str:
    version = hashlib.sha256(VERSION_FORMAT)
    for directory in dirs:
        version.update(b"dir\0%s\n" % encode(directory))
    for file in data:
        version.update(b"file\0%o\0%s\0%s\n" % (archive_mode(file.mode), file.digest.encode(), encode(file.path)))
    for library in libraries:
        version.update(b"lib\0%s\0%s\n" % (library.digest.encode(), library.name.encode()))
    version.update(b"symlinks\0" + symlinks)
    return version.hexdigest()


def archive_members(data: Iterable[File], dirs: Iterable[str], symlinks: bytes, version: str) -> list[Member]:
    members = [Member(f"{directory}/", stat.S_IFDIR | DIR_MODE) for directory in dirs]
    members += [Member(file.path, stat.S_IFREG | archive_mode(file.mode), source=file.source) for file in data]
    members.append(Member(SYMLINKS, stat.S_IFREG | 0o600, data=symlinks))
    members.append(Member(VERSION, stat.S_IFREG | 0o600, data=f"{version}\n".encode()))
    return sorted(members, key=lambda member: member.name)


def zip_info(member: Member) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(member.name, date_time=ZIP_TIME)
    info.create_system = ZIP_UNIX
    is_dir = stat.S_ISDIR(member.mode)
    info.external_attr = member.mode << 16 | (ZIP_DOS_DIRECTORY if is_dir else 0)
    info.compress_type = zipfile.ZIP_STORED if is_dir else zipfile.ZIP_DEFLATED
    return info


@contextmanager
def replacing(destination: Path, mode: int) -> Iterator[Path]:
    destination.parent.mkdir(parents=True, exist_ok=True)
    handle, name = tempfile.mkstemp(dir=destination.parent, prefix=f".{destination.name}.")
    os.close(handle)
    temporary = Path(name)
    try:
        yield temporary
        temporary.chmod(mode)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def write_archive(destination: Path, members: Iterable[Member]) -> None:
    with replacing(destination, ARCHIVE_MODE) as temporary, zipfile.ZipFile(temporary, "w") as archive:
        for member in members:
            archive.writestr(zip_info(member), member.read(), compresslevel=ZIP_LEVEL)


def remove_stale_libraries(directory: Path, wanted: set[str]) -> None:
    for stale in sorted(directory.glob(f"{LIB_PREFIX}*{LIB_SUFFIX}")):
        if stale.name not in wanted and not stale.is_dir():
            stale.unlink()


def write_libraries(directory: Path, libraries: list[Library]) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    remove_stale_libraries(directory, {library.name for library in libraries})
    for library in libraries:
        with replacing(directory / library.name, LIB_MODE) as temporary:
            shutil.copyfile(library.source, temporary)


def package(prefix: Path, abi: str, jnilibs: Path, assets: Path) -> Summary:
    if abi not in ABI_MACHINES:
        raise PackageError(f"unknown ABI {abi}; expected one of {', '.join(sorted(ABI_MACHINES))}")
    if not prefix.is_dir():
        raise PackageError(f"{prefix}: not a directory")
    scanned = scan(prefix)
    check_machines(scanned.files, abi)
    libraries = group_executables(scanned.files)
    data = [file for file in scanned.files if not file.kind.is_executable]
    warn_special_bits(scanned.files)
    links = all_symlinks(scanned.symlinks, libraries)
    symlinks = render_symlinks(links)
    dirs = bare_dirs(scanned.dirs, (file.path for file in data))
    version = userland_version(data, dirs, libraries, symlinks)
    write_libraries(jnilibs / abi, libraries)
    write_archive(assets / "userland" / f"{abi}.zip", archive_members(data, dirs, symlinks, version))
    executables = sum(len(library.paths) for library in libraries)
    return Summary(
        abi, len(data), executables, len(libraries), len(links), len(scanned.symlinks), len(dirs), version
    )


def contained(root: Path, relative: str) -> Path:
    parts = PurePosixPath(relative).parts
    if not parts or PurePosixPath(relative).is_absolute() or ".." in parts:
        raise PackageError(f"{relative!r}: escapes the prefix")
    return root.joinpath(*parts)


def parse_symlinks(text: str) -> list[tuple[str, str]]:
    links = []
    for line in text.splitlines():
        fields = line.split(LINK_ARROW)
        if len(fields) != 2:
            raise PackageError(f"{SYMLINKS}: malformed line {line!r}")
        target, path = fields
        links.append((target, path.removeprefix("./")))
    return links


def extract_member(archive: zipfile.ZipFile, info: zipfile.ZipInfo, dest: Path) -> None:
    target = contained(dest, info.filename)
    if info.is_dir():
        target.mkdir(parents=True, exist_ok=True)
    else:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(archive.read(info))
    target.chmod(stat.S_IMODE(info.external_attr >> 16))


def refresh_applib(link: Path, native_lib_dir: Path) -> None:
    if link.is_symlink():
        link.unlink()
    link.symlink_to(native_lib_dir.resolve(), target_is_directory=True)


def install_into(dest: Path, archive: Path, native_lib_dir: Path) -> None:
    dest.mkdir(parents=True)
    with zipfile.ZipFile(archive) as bundle:
        for info in bundle.infolist():
            if info.filename != SYMLINKS:
                extract_member(bundle, info, dest)
        links = parse_symlinks(bundle.read(SYMLINKS).decode("utf-8", "surrogateescape"))
    for target, path in links:
        link = contained(dest, path)
        link.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(target, link)
    refresh_applib(dest.parent / APPLIB, native_lib_dir)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Split a userland prefix into jniLibs executables and an asset zip for the amux app."
    )
    parser.add_argument("--prefix", type=Path, required=True, help="the contents of $PREFIX")
    parser.add_argument("--abi", required=True, choices=sorted(ABI_MACHINES))
    parser.add_argument("--jnilibs", type=Path, required=True, help="jniLibs root; writes <jnilibs>/<abi>/libu_*.so")
    parser.add_argument("--assets", type=Path, required=True, help="assets root; writes <assets>/userland/<abi>.zip")
    args = parser.parse_args(argv)
    logging.basicConfig(format="package.py: %(message)s", level=logging.INFO)
    try:
        summary = package(args.prefix, args.abi, args.jnilibs, args.assets)
    except PackageError as error:
        log.error("%s", error)
        return 1
    print(
        f"{summary.abi}: {summary.files} files and {summary.bare_dirs} bare directories in the zip, "
        f"{summary.executables} executables as {summary.libraries} libraries, "
        f"{summary.symlinks} symlinks ({summary.original_symlinks} from the prefix)"
    )
    print(f"{summary.abi}: {VERSION} {summary.version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
