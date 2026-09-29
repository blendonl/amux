import contextlib
import hashlib
import io
import os
import re
import stat
import tempfile
import unittest
import zipfile
from pathlib import Path

from support import (
    DEVICE_PREFIX,
    EM_AARCH64,
    ET_REL,
    Regular,
    Symlink,
    build,
    elf,
    executables_in_archive,
    outputs,
    package,
    reversed_build_order,
    round_trip_problems,
    sample_nodes,
)

ABI = "x86_64"
ANDROID_LIBRARY_NAME = re.compile(r"^lib[A-Za-z0-9._-]+\.so$")


def classify_bytes(data: bytes, path: str = "bin/tool", mode: int = 0o100755) -> package.Kind:
    return package.classify(path, mode, package.read_elf(io.BytesIO(data)))


def short_hash(path: str) -> str:
    return hashlib.sha256(path.encode()).hexdigest()[:8]


class ClassifyTests(unittest.TestCase):
    def test_et_exec_is_an_executable(self) -> None:
        self.assertIs(classify_bytes(elf(package.ET_EXEC, interp=False)), package.Kind.EXECUTABLE)

    def test_pie_with_an_interpreter_is_an_executable(self) -> None:
        self.assertIs(classify_bytes(elf()), package.Kind.EXECUTABLE)

    def test_executables_outside_bin_are_found(self) -> None:
        self.assertIs(classify_bytes(elf(), path="lib/apt/methods/http", mode=0o100644), package.Kind.EXECUTABLE)

    def test_shared_library_without_an_interpreter_stays(self) -> None:
        self.assertIs(classify_bytes(elf(interp=False, entry=0), path="lib/libz.so"), package.Kind.LIBRARY)

    def test_static_pie_in_bin_or_libexec_is_an_executable(self) -> None:
        static_pie = elf(interp=False, entry=0x2000)
        self.assertIs(classify_bytes(static_pie, path="bin/toybox"), package.Kind.STATIC_PIE)
        self.assertIs(classify_bytes(static_pie, path="libexec/git-core/helper"), package.Kind.STATIC_PIE)
        self.assertTrue(package.Kind.STATIC_PIE.is_executable)

    def test_static_pie_rule_needs_exec_dir_exec_bit_and_entry(self) -> None:
        static_pie = elf(interp=False, entry=0x2000)
        self.assertIs(classify_bytes(static_pie, path="lib/libc++_shared.so"), package.Kind.LIBRARY)
        self.assertIs(classify_bytes(static_pie, path="bin/tool", mode=0o100644), package.Kind.LIBRARY)
        self.assertIs(classify_bytes(static_pie, path="bin"), package.Kind.LIBRARY)
        self.assertIs(classify_bytes(elf(interp=False, entry=0), path="bin/tool"), package.Kind.LIBRARY)

    def test_big_endian_elf64_is_parsed(self) -> None:
        self.assertIs(classify_bytes(elf(order=">")), package.Kind.EXECUTABLE)
        self.assertIs(classify_bytes(elf(order=">", interp=False, entry=0), path="lib/x.so"), package.Kind.LIBRARY)
        header = package.read_elf(io.BytesIO(elf(order=">", machine=EM_AARCH64, entry=0x1234)))
        self.assertEqual(header, package.Elf(package.ET_DYN, EM_AARCH64, 0x1234, True))

    def test_elf32_is_parsed_without_crashing(self) -> None:
        self.assertIs(classify_bytes(elf(elf_class=1)), package.Kind.EXECUTABLE)
        self.assertIs(classify_bytes(elf(elf_class=1, order=">")), package.Kind.EXECUTABLE)
        self.assertIs(classify_bytes(elf(elf_class=1, interp=False, entry=0), path="lib/x.so"), package.Kind.LIBRARY)

    def test_relocatable_object_is_data(self) -> None:
        self.assertIs(classify_bytes(elf(ET_REL, interp=False, entry=0)), package.Kind.DATA)

    def test_truncated_header_is_malformed(self) -> None:
        for length in (17, 40, 63):
            with self.subTest(length=length), self.assertRaises(package.MalformedElf):
                package.read_elf(io.BytesIO(elf()[:length]))

    def test_truncated_program_headers_are_malformed(self) -> None:
        with self.assertRaises(package.MalformedElf):
            package.read_elf(io.BytesIO(elf()[:100]))

    def test_unknown_class_or_byte_order_is_malformed(self) -> None:
        for ident in (b"\x03\x01", b"\x02\x03"):
            with self.subTest(ident=ident), self.assertRaises(package.MalformedElf):
                package.read_elf(io.BytesIO(package.ELF_MAGIC + ident + bytes(58)))

    def test_non_elf_files_are_data(self) -> None:
        for data in (b"", b"\x7fEL", b"#!/bin/sh\n", b"\x7fELG" + bytes(60)):
            with self.subTest(data=data):
                self.assertIsNone(package.read_elf(io.BytesIO(data)))
                self.assertIs(classify_bytes(data), package.Kind.DATA)

    def test_malformed_files_are_kept_as_data_with_a_warning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            broken = Path(temporary, "broken")
            broken.write_bytes(elf()[:30])
            with self.assertLogs("package", "WARNING") as logs:
                self.assertIsNone(package.inspect_elf("bin/broken", broken))
        self.assertIn("bin/broken: truncated ELF header", logs.output[0])


class NameTests(unittest.TestCase):
    def test_paths_are_flattened_and_sanitized(self) -> None:
        self.assertEqual(package.sanitize("bin/zsh"), "bin_zsh")
        self.assertEqual(package.sanitize("libexec/git-core/git-remote-http"), "libexec_git-core_git-remote-http")
        self.assertEqual(package.sanitize("bin/my tool@2+ü"), "bin_my_tool_2__")

    def test_unique_paths_get_plain_names(self) -> None:
        names = package.library_names(["bin/zsh", "bin/git"])
        self.assertEqual(names, {"bin/zsh": "libu_bin_zsh.so", "bin/git": "libu_bin_git.so"})

    def test_colliding_names_get_distinct_stable_hashes(self) -> None:
        names = package.library_names(["bin/c++", "bin/c__", "bin/zsh"])
        self.assertEqual(names["bin/c++"], f"libu_bin_c___{short_hash('bin/c++')}.so")
        self.assertEqual(names["bin/c__"], f"libu_bin_c___{short_hash('bin/c__')}.so")
        self.assertEqual(names["bin/zsh"], "libu_bin_zsh.so")
        self.assertEqual(package.library_names(["bin/zsh", "bin/c__", "bin/c++"]), names)

    def test_long_names_are_cut_and_hashed(self) -> None:
        path = "share/" + "x" * 300
        name = package.library_names([path])[path]
        self.assertEqual(len(name), package.MAX_LIB_NAME)
        self.assertTrue(name.endswith(f"_{short_hash(path)}.so"))

    def test_names_are_valid_android_library_names(self) -> None:
        paths = ["bin/zsh", "bin/c++", "bin/c__", "bin/my tool@2", "libexec/ü/x", "share/" + "y" * 300]
        for name in package.library_names(paths).values():
            self.assertRegex(name, ANDROID_LIBRARY_NAME)

    def test_the_smallest_duplicate_path_names_the_library(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            prefix = build(
                Path(temporary),
                [Regular(path, elf(tag=b"same"), 0o755) for path in ("libexec/z", "bin/zz", "bin/z")],
            )
            (library,) = package.group_executables(package.scan(prefix).files)
        self.assertEqual(library.name, "libu_bin_z.so")
        self.assertEqual(library.paths, ("bin/z", "bin/zz", "libexec/z"))


class PackagedPrefix(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.prefix = build(self.root / "prefix", sample_nodes())
        self.jnilibs = self.root / "out" / "jniLibs"
        self.assets = self.root / "out" / "assets"
        with self.assertLogs("package", "INFO") as self.logs:
            self.summary = package.package(self.prefix, ABI, self.jnilibs, self.assets)
        self.libs = self.jnilibs / ABI
        self.zip_path = self.assets / "userland" / f"{ABI}.zip"
        self.archive = zipfile.ZipFile(self.zip_path)
        self.addCleanup(self.archive.close)

    def symlinks(self) -> list[str]:
        return self.archive.read(package.SYMLINKS).decode().splitlines()

    def repackage(self, name: str, prefix: Path | None = None) -> dict[str, tuple[int, bytes]]:
        out = self.root / name
        with self.assertLogs("package", "INFO"):
            package.package(prefix or self.prefix, ABI, out / "jniLibs", out / "assets")
        return outputs(out)


class PackageTests(PackagedPrefix):
    def test_every_distinct_executable_ships_once(self) -> None:
        expected = {
            "libu_bin_zsh.so",
            "libu_bin_git.so",
            "libu_libexec_git-core_git-remote-http.so",
            "libu_libexec_a_b_c_tool.so",
            "libu_libexec_only-exec_runner.so",
            "libu_bin_ls.so",
            "libu_bin_toybox.so",
            f"libu_bin_c___{short_hash('bin/c++')}.so",
            f"libu_bin_c___{short_hash('bin/c__')}.so",
            "libu_bin_my_tool_2.so",
            "libu_bin_nano.so",
        }
        self.assertEqual({path.name for path in self.libs.iterdir()}, expected)
        self.assertEqual(self.summary.libraries, len(expected))
        self.assertEqual(self.summary.executables, len(expected) + 2)

    def test_libraries_hold_the_original_bytes(self) -> None:
        self.assertEqual((self.libs / "libu_bin_zsh.so").read_bytes(), (self.prefix / "bin/zsh").read_bytes())
        self.assertEqual((self.libs / "libu_bin_git.so").read_bytes(), elf(tag=b"git"))
        for library in self.libs.iterdir():
            self.assertEqual(stat.S_IMODE(library.stat().st_mode), package.LIB_MODE)

    def test_zip_holds_no_executables_and_no_symlinks(self) -> None:
        self.assertEqual(executables_in_archive(self.zip_path), [])
        names = set(self.archive.namelist())
        for info in self.archive.infolist():
            self.assertFalse(stat.S_ISLNK(info.external_attr >> 16), info.filename)
        for path in ("bin/zsh", "bin/zsh-5.9", "bin/toybox", "bin/vi", "lib/libz.so", "var/cache/links/current"):
            self.assertNotIn(path, names)

    def test_zip_holds_every_other_file_with_an_owner_only_mode(self) -> None:
        expected = {
            "bin/zcat": 0o700,
            "bin/no-exec-bit": 0o600,
            "lib/libz.so.1.3": 0o600,
            "lib/libc++_shared.so": 0o700,
            "lib/zsh/5.9/zsh/zle.so": 0o700,
            "share/doc/zsh/README": 0o600,
            "share/broken.elf": 0o600,
            "share/objects/start.o": 0o600,
            "etc/profile": 0o600,
            "libexec/suid-helper": 0o700,
            package.SYMLINKS: 0o600,
            package.VERSION: 0o600,
        }
        files = {info.filename: info.external_attr >> 16 for info in self.archive.infolist() if not info.is_dir()}
        self.assertEqual(files, {name: stat.S_IFREG | mode for name, mode in expected.items()})
        self.assertEqual(self.archive.read("share/doc/zsh/README"), b"zsh docs\n")
        self.assertEqual(self.summary.files, len(expected) - 2)

    def test_bare_directories_are_explicit_entries(self) -> None:
        dirs = {info.filename: info for info in self.archive.infolist() if info.is_dir()}
        expected = {
            "var/empty/",
            "var/cache/links/",
            "tmp/",
            "libexec/a/b/c/",
            "libexec/git-core/",
            "libexec/only-exec/",
        }
        self.assertEqual(set(dirs), expected)
        for info in dirs.values():
            self.assertEqual(info.external_attr, (stat.S_IFDIR | 0o700) << 16 | 0x10)
            self.assertEqual(info.compress_type, zipfile.ZIP_STORED)

    def test_symlinks_txt_lists_original_and_applib_links(self) -> None:
        expected = [
            f"../../applib/libu_bin_c___{short_hash('bin/c++')}.so←./bin/c++",
            f"../../applib/libu_bin_c___{short_hash('bin/c__')}.so←./bin/c__",
            "../../applib/libu_bin_git.so←./bin/git",
            "../../applib/libu_bin_ls.so←./bin/ls",
            "../../applib/libu_bin_my_tool_2.so←./bin/my tool@2",
            "../../applib/libu_bin_nano.so←./bin/nano",
            "/system/bin/sh←./bin/sh",
            "../../applib/libu_bin_toybox.so←./bin/toybox",
            "nano←./bin/vi",
            f"{DEVICE_PREFIX}/bin/nano←./bin/view",
            "../../applib/libu_bin_zsh.so←./bin/zsh",
            "../../applib/libu_bin_zsh.so←./bin/zsh-5.9",
            "libz.so.1.3←./lib/libz.so",
            "../../../../../applib/libu_libexec_a_b_c_tool.so←./libexec/a/b/c/tool",
            "../../../applib/libu_bin_git.so←./libexec/git-core/git",
            "../../../applib/libu_libexec_git-core_git-remote-http.so←./libexec/git-core/git-remote-http",
            "../../../applib/libu_libexec_only-exec_runner.so←./libexec/only-exec/runner",
            "../../../etc/profile←./var/cache/links/current",
        ]
        self.assertEqual(self.symlinks(), expected)
        self.assertEqual(self.summary.symlinks, len(expected))
        self.assertEqual(self.summary.original_symlinks, 5)

    def test_zip_entries_are_sorted_and_normalized(self) -> None:
        names = self.archive.namelist()
        self.assertEqual(names, sorted(names))
        for info in self.archive.infolist():
            self.assertEqual(info.date_time, package.ZIP_TIME)
            self.assertEqual(info.create_system, 3)

    def test_version_file_holds_the_summary_version(self) -> None:
        self.assertEqual(self.archive.read(package.VERSION), f"{self.summary.version}\n".encode())
        self.assertRegex(self.summary.version, r"^[0-9a-f]{64}$")

    def test_static_pies_setuid_bits_and_malformed_elves_are_logged(self) -> None:
        output = "\n".join(self.logs.output)
        self.assertIn("bin/toybox: an executable ELF without an interpreter; shipping it as a static-pie", output)
        self.assertIn("libexec/suid-helper: dropping setuid", output)
        self.assertIn("share/broken.elf: truncated ELF header", output)

    def test_machine_mismatch_is_logged(self) -> None:
        prefix = build(self.root / "arm", [Regular("bin/zsh", elf(machine=EM_AARCH64), 0o755)])
        with self.assertLogs("package", "WARNING") as logs:
            package.package(prefix, ABI, self.root / "arm-out" / "jniLibs", self.root / "arm-out" / "assets")
        self.assertIn("bin/zsh: built for ELF machine 183, not x86_64", logs.output[0])


class OutputDirectoryTests(PackagedPrefix):
    def test_stale_libraries_go_and_other_files_stay(self) -> None:
        (self.libs / "libu_gone.so").write_bytes(b"old")
        (self.libs / "libu_bin_zsh.so").write_bytes(b"stale")
        (self.libs / "libamux.so").write_bytes(b"amux")
        (self.libs / "notes.txt").write_bytes(b"keep")
        (self.assets / "userland" / "arm64-v8a.zip").write_bytes(b"other abi")
        with self.assertLogs("package", "INFO"):
            package.package(self.prefix, ABI, self.jnilibs, self.assets)
        self.assertFalse((self.libs / "libu_gone.so").exists())
        self.assertEqual((self.libs / "libu_bin_zsh.so").read_bytes(), (self.prefix / "bin/zsh").read_bytes())
        self.assertEqual((self.libs / "libamux.so").read_bytes(), b"amux")
        self.assertEqual((self.libs / "notes.txt").read_bytes(), b"keep")
        self.assertEqual((self.assets / "userland" / "arm64-v8a.zip").read_bytes(), b"other abi")

    def test_no_temporary_files_are_left(self) -> None:
        self.assertEqual([path.name for path in self.zip_path.parent.iterdir()], [f"{ABI}.zip"])
        self.assertFalse([path for path in self.libs.iterdir() if path.name.startswith(".")])


class DeterminismTests(PackagedPrefix):
    def test_a_second_run_is_byte_identical(self) -> None:
        self.assertEqual(self.repackage("again"), outputs(self.root / "out"))

    def test_a_rerun_in_place_is_byte_identical(self) -> None:
        before = outputs(self.root / "out")
        with self.assertLogs("package", "INFO"):
            package.package(self.prefix, ABI, self.jnilibs, self.assets)
        self.assertEqual(outputs(self.root / "out"), before)

    def test_umask_and_creation_order_do_not_matter(self) -> None:
        previous = os.umask(0o077)
        try:
            rebuilt = build(self.root / "reversed", reversed_build_order(sample_nodes()))
            result = self.repackage("reversed-out", rebuilt)
        finally:
            os.umask(previous)
        self.assertEqual(result, outputs(self.root / "out"))

    def version_after(self, change) -> str:
        change(self.prefix)
        with self.assertLogs("package", "INFO"):
            return package.package(self.prefix, ABI, self.jnilibs, self.assets).version

    def test_the_version_follows_every_kind_of_change(self) -> None:
        changes = {
            "data": lambda prefix: (prefix / "etc/profile").write_bytes(b"export PATH HOME\n"),
            "mode": lambda prefix: (prefix / "etc/profile").chmod(0o755),
            "executable": lambda prefix: (prefix / "bin/nano").write_bytes(elf(tag=b"nano 2")),
            "symlink": lambda prefix: Symlink("bin/ex", "nano").create(prefix),
            "directory": lambda prefix: (prefix / "var/run").mkdir(),
            "dedupe": lambda prefix: (prefix / "bin/zsh-5.9").unlink(),
        }
        versions = [self.summary.version]
        for name, change in changes.items():
            with self.subTest(change=name):
                versions.append(self.version_after(change))
                self.assertNotIn(versions[-1], versions[:-1])


class RejectionTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def package_nodes(self, nodes, abi: str = ABI) -> None:
        prefix = build(self.root / "prefix", nodes)
        package.package(prefix, abi, self.root / "jniLibs", self.root / "assets")

    def test_reserved_names_are_rejected(self) -> None:
        for name in (package.SYMLINKS, package.VERSION):
            with self.subTest(name=name), self.assertRaisesRegex(package.PackageError, "reserved"):
                self.package_nodes([Regular(name, b"x")])

    def test_names_that_break_symlinks_txt_are_rejected(self) -> None:
        cases = [[Regular("bin/a←b", b"x")], [Regular("bin/a\nb", b"x")], [Symlink("bin/a", "b←c")]]
        for nodes in cases:
            with self.subTest(nodes=nodes), self.assertRaisesRegex(package.PackageError, package.SYMLINKS):
                self.package_nodes(nodes)

    def test_unknown_abis_and_missing_prefixes_are_rejected(self) -> None:
        with self.assertRaisesRegex(package.PackageError, "unknown ABI"):
            self.package_nodes([], abi="mips")
        with self.assertRaisesRegex(package.PackageError, "not a directory"):
            package.package(self.root / "missing", ABI, self.root / "jniLibs", self.root / "assets")


class CommandLineTests(unittest.TestCase):
    def test_main_writes_both_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = build(root / "prefix", [Regular("bin/zsh", elf(), 0o755), Regular("etc/zshrc", b"")])
            arguments = ["--prefix", str(prefix), "--abi", ABI]
            arguments += ["--jnilibs", str(root / "j"), "--assets", str(root / "a")]
            printed = io.StringIO()
            with contextlib.redirect_stdout(printed):
                self.assertEqual(package.main(arguments), 0)
            self.assertTrue((root / "j" / ABI / "libu_bin_zsh.so").is_file())
            self.assertTrue((root / "a" / "userland" / f"{ABI}.zip").is_file())
        summary = "x86_64: 1 files and 1 bare directories in the zip, 1 executables as 1 libraries, 1 symlinks"
        self.assertIn(summary, printed.getvalue())

    def test_main_reports_errors(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            arguments = ["--prefix", str(Path(temporary, "missing")), "--abi", ABI]
            arguments += ["--jnilibs", temporary, "--assets", temporary]
            with self.assertLogs("package", "ERROR"):
                self.assertEqual(package.main(arguments), 1)


class RoundTripTests(PackagedPrefix):
    def setUp(self) -> None:
        super().setUp()
        self.files_dir = self.root / "files"
        self.installed = self.files_dir / "usr"
        package.install_into(self.installed, self.zip_path, self.libs)

    def test_every_original_path_resolves_to_the_same_bytes(self) -> None:
        self.assertEqual(round_trip_problems(self.prefix, self.installed), [])

    def test_executables_resolve_into_applib(self) -> None:
        applib = self.files_dir / package.APPLIB
        self.assertEqual(os.readlink(applib), str(self.libs.resolve()))
        for path in ("bin/zsh", "bin/zsh-5.9", "libexec/a/b/c/tool", "bin/my tool@2"):
            link = self.installed / path
            self.assertTrue(link.is_symlink(), path)
            self.assertEqual(link.resolve().parent, self.libs.resolve())
            self.assertTrue(os.access(link, os.X_OK), path)

    def test_installed_modes_follow_the_zip(self) -> None:
        self.assertEqual(stat.S_IMODE((self.installed / "bin/zcat").stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((self.installed / "etc/profile").stat().st_mode), 0o600)
        self.assertTrue((self.installed / "var/empty").is_dir())
        self.assertEqual((self.installed / package.VERSION).read_text(), f"{self.summary.version}\n")

    def test_applib_is_refreshed_on_every_start(self) -> None:
        moved = self.root / "moved-libs"
        self.libs.rename(moved)
        package.refresh_applib(self.files_dir / package.APPLIB, moved)
        self.assertEqual(round_trip_problems(self.prefix, self.installed), [])

    def test_archives_that_escape_the_prefix_are_refused(self) -> None:
        evil = self.root / "evil.zip"
        with zipfile.ZipFile(evil, "w") as archive:
            archive.writestr(package.SYMLINKS, "x←./../../escape\n")
        with self.assertRaisesRegex(package.PackageError, "escapes"):
            package.install_into(self.root / "evil" / "usr", evil, self.libs)
        self.assertFalse((self.root / "escape").exists())


if __name__ == "__main__":
    unittest.main()
