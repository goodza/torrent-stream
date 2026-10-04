"""Bundle native dependencies, relocate, smoke-test, then archive a release."""
import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def run(*args, **kwargs):
    return subprocess.run(
        [str(arg) for arg in args], check=True, text=True, encoding="utf-8",
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs
    ).stdout


def linux_bundle(binary, root):
    libraries = root / "lib"
    libraries.mkdir()
    # Keep glibc and its loader supplied by the host; bundle the C++/torrent/TLS libraries.
    system = re.compile(r"^(ld-linux.*|lib(c|m|dl|pthread|rt|resolv|util)\.so.*)$")
    for line in run("ldd", binary).splitlines():
        if "not found" in line:
            raise RuntimeError(f"Unresolved native dependency: {line}")
        match = re.match(r"\s*(\S+) => (/\S+)", line)
        if not match or system.match(match[1]):
            continue
        source = Path(match[2])
        destination = libraries / match[1]
        shutil.copy2(source, destination, follow_symlinks=True)
        run("patchelf", "--set-rpath", "$ORIGIN", destination)
        try:
            package = run("dpkg-query", "-S", source).split(": ", 1)[0].split(":", 1)[0]
        except subprocess.CalledProcessError:
            continue  # A custom native prefix uses the included upstream notices.
        copyright_file = Path("/usr/share/doc") / package / "copyright"
        if copyright_file.exists():
            shutil.copy2(copyright_file, root / "licenses" / f"{package}.txt")
    shutil.copy2("/usr/share/common-licenses/GPL-3", root / "licenses" / "GPL-3.txt")
    run("patchelf", "--set-rpath", "$ORIGIN/lib", binary)


def macos_bundle(binary, root):
    libraries = root / "lib"
    libraries.mkdir()
    pending = [(binary, binary)]
    copied = {}
    while pending:
        original, destination = pending.pop()
        # otool prints a dylib's own install ID before its dependencies.
        first_dependency = 1 if original == binary else 2
        dependencies = [line.strip().split(" (", 1)[0]
                        for line in run("otool", "-L", original).splitlines()[first_dependency:]]
        for dependency in dependencies:
            if dependency.startswith(("/usr/lib/", "/System/Library/")):
                continue
            if dependency.startswith("@"):
                # Resolve loader-relative paths against the original, before rewriting.
                if dependency.startswith("@loader_path/"):
                    source = original.parent / dependency[len("@loader_path/"):]
                elif dependency.startswith("@rpath/"):
                    rpaths = re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset",
                                        run("otool", "-l", original))
                    candidates = [Path(p.replace("@loader_path", str(original.parent))) /
                                  dependency[len("@rpath/"):] for p in rpaths]
                    source = next((p for p in candidates if p.exists()), None)
                    if source is None:
                        raise RuntimeError(f"Cannot resolve {dependency} in {original}")
                else:
                    raise RuntimeError(f"Unsupported native dependency {dependency}")
            else:
                source = Path(dependency)
            source = source.resolve(strict=True)
            if source == original.resolve():  # A dylib's own install ID.
                continue
            name = source.name
            if name in copied and copied[name] != source:
                raise RuntimeError(f"Conflicting dylib names: {name}")
            if name not in copied:
                copied[name] = source
                bundled = libraries / name
                shutil.copy2(source, bundled)
                bundled.chmod(0o755)
                pending.append((source, bundled))
                run("install_name_tool", "-id", f"@executable_path/lib/{name}", bundled)
            run("install_name_tool", "-change", dependency,
                f"@executable_path/lib/{name}", destination)
    for library in libraries.iterdir():
        run("codesign", "--force", "--sign", "-", library)
    run("codesign", "--force", "--sign", "-", binary)


def windows_bundle(root):
    prefix = Path(os.environ["VCPKG_ROOT"]) / "installed" / os.environ["VCPKGRS_TRIPLET"]
    dlls = list((prefix / "bin").glob("*.dll"))
    if not dlls:
        raise RuntimeError("No native DLLs found in vcpkg installation")
    for dll in dlls:
        shutil.copy2(dll, root / dll.name)
    for copyright_file in (prefix / "share").glob("*/copyright"):
        shutil.copy2(copyright_file, root / "licenses" / f"{copyright_file.parent.name}.txt")


def bencode(value):
    if isinstance(value, int):
        return b"i" + str(value).encode() + b"e"
    if isinstance(value, bytes):
        return str(len(value)).encode() + b":" + value
    return b"d" + b"".join(bencode(k) + bencode(v) for k, v in sorted(value.items())) + b"e"


def smoke_test(binary):
    env = os.environ.copy()
    for name in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "LIBTORRENT_PREFIX"):
        env.pop(name, None)
    # Prevent the installed vcpkg DLLs from hiding missing bundled DLLs.
    if sys.platform == "win32":
        system_root = Path(os.environ["SystemRoot"])
        env["PATH"] = os.pathsep.join(str(p) for p in (system_root / "System32", system_root))
    run(binary, "--version", env=env)
    with tempfile.TemporaryDirectory(prefix="torrent-stream-検証-") as directory:
        directory = Path(directory)
        torrent = directory / "映画.torrent"
        name = "Фильм.mkv"
        torrent.write_bytes(bencode({b"info": {
            b"length": 262144, b"name": name.encode("utf-8"),
            b"piece length": 262144, b"pieces": hashlib.sha1(bytes(262144)).digest()
        }}))
        output = run(binary, torrent, "--list-files", "--path", directory, env=env)
        if name not in output:
            raise RuntimeError(f"Native Unicode metadata smoke test failed: {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    name = f"torrent-stream-v{args.version}-{args.target}"
    executable = "torrent-stream.exe" if sys.platform == "win32" else "torrent-stream"
    dist = Path("dist").resolve()
    dist.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory() as staging:
        root = Path(staging) / name
        root.mkdir()
        binary = root / executable
        shutil.copy2(Path("target") / args.target / "release" / executable, binary)
        for document in ("README.md", "LICENSE"):
            shutil.copy2(document, root / document)
        shutil.copytree("ci/licenses", root / "licenses")
        if sys.platform == "linux":
            linux_bundle(binary, root)
        elif sys.platform == "darwin":
            macos_bundle(binary, root)
        elif sys.platform == "win32":
            windows_bundle(root)
        else:
            raise RuntimeError(f"Unsupported release platform: {sys.platform}")
        archive = shutil.make_archive(str(dist / name),
                                     "zip" if sys.platform == "win32" else "gztar",
                                     root_dir=staging, base_dir=name)
    # Smoke-test the archive after extraction into a different directory.
    with tempfile.TemporaryDirectory() as relocated:
        shutil.unpack_archive(archive, relocated)
        smoke_test(Path(relocated) / name / executable)
    print(f"Packaged and verified {archive}")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stdout, file=sys.stderr)
        print(error.stderr, file=sys.stderr)
        raise
