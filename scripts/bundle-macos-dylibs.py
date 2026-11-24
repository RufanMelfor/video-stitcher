#!/usr/bin/env python3
"""Copy the non-system dylibs an app bundle needs into Contents/Frameworks
and rewrite load paths so the bundle is relocatable.

usage: bundle-macos-dylibs.py <Frameworks dir> <lib prefix> <exe> [<exe> ...]

Every dependency that lives under <lib prefix> (our self-built FFmpeg) is
copied (symlinks resolved) into <Frameworks dir>, given an
@rpath/<name> install name, and the executables get an
@executable_path/../Frameworks rpath. Anything that is neither a system
library (/usr/lib, /System) nor bundled aborts with an error, so an app
that would fail on a Mac without Homebrew can't be produced silently.
Code signatures are invalidated; the caller must re-sign.
"""
import os, shutil, subprocess, sys

def deps(path):
    out = subprocess.run(["otool", "-L", path], capture_output=True, text=True, check=True).stdout
    return [l.split()[0] for l in out.splitlines()[1:] if l.strip()]

def is_system(dep):
    return dep.startswith(("/usr/lib/", "/System/", "@rpath/", "@executable_path/", "@loader_path/"))

def run(*cmd):
    subprocess.run(cmd, check=True, capture_output=True)

def main():
    fw, prefix, exes = sys.argv[1], os.path.realpath(sys.argv[2]), sys.argv[3:]
    os.makedirs(fw, exist_ok=True)
    copied = {}  # basename -> path in bundle
    queue = list(exes)
    todo = []
    while queue:
        f = queue.pop()
        todo.append(f)
        for d in deps(f):
            if is_system(d):
                continue
            real = os.path.realpath(d)
            if not real.startswith(prefix + os.sep):
                sys.exit(f"error: {f} depends on non-bundled library {d}")
            base = os.path.basename(d)  # keep the soname (libavcodec.61.dylib)
            if base not in copied:
                dest = os.path.join(fw, base)
                shutil.copy2(real, dest)
                os.chmod(dest, 0o755)
                copied[base] = dest
                queue.append(dest)
    for f in todo:
        is_lib = f.startswith(fw)
        if is_lib:
            run("install_name_tool", "-id", "@rpath/" + os.path.basename(f), f)
        for d in deps(f):
            if not is_system(d):
                run("install_name_tool", "-change", d, "@rpath/" + os.path.basename(d), f)
        if not is_lib:
            rp = subprocess.run(["otool", "-l", f], capture_output=True, text=True).stdout
            if "@executable_path/../Frameworks" not in rp:
                run("install_name_tool", "-add_rpath", "@executable_path/../Frameworks", f)
    # verify closure
    for f in todo:
        for d in deps(f):
            if not is_system(d):
                sys.exit(f"error: {f} still references {d}")
            if d.startswith("@rpath/") and not os.path.exists(os.path.join(fw, d[7:])):
                sys.exit(f"error: {f} needs {d}, not bundled")
    print(f"bundled {len(copied)} libraries into {fw}")


if __name__ == "__main__":
    main()
