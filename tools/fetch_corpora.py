#!/usr/bin/env python3
"""Fetch the public test corpora the codec tests run against.

The files are data, used under their own licences and never committed here
(see docs/VALIDATION.md). Each file is checked against tools/corpora.sha256;
anything that does not match is refused.

    python3 tools/fetch_corpora.py [DEST]          # default DEST: ./corpora
    python3 tools/fetch_corpora.py DEST --update-manifest

The tests find the corpora through the RIVET_IMAGE_CORPORA environment
variable, falling back to ./corpora at the workspace root; without them the
corpus tests skip (and say so).

Corpora:
  bmpsuite      Jason Summers' BMP Suite 2.8 (images GPL-3.0 as part of the
                suite; reference PNGs from its html/ directory)
  libtiff-pics  libtiff's sample images, pics-3.8.0 (various; depth/ is
                public domain)
  libtiff-test  libtiff's test/images at tag v4.6.0 (libtiff licence),
                with the PNM references some of them have
  gif-suite     Robert Ancell's GIF test suite from pygif at a pinned
                commit (CC-BY-SA-4.0): GIFs, .conf expectations and raw .rgba
                reference frames. Only the test-suite/ data is extracted.
"""

import fnmatch
import hashlib
import io
import os
import sys
import tarfile
import urllib.request
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "corpora.sha256")

PYGIF_COMMIT = "741b282047e0dde112e5ac2df0c7f8f923cb5df6"
LIBTIFF_TAG = "v4.6.0"
LIBTIFF_TEST_FILES = [
    "README.txt",
    "custom_dir_EXIF_GPS.tiff",
    "deflate-last-strip-extra-data.tiff",
    "logluv-3c-16b.tiff",
    "lzw-single-strip.tiff",
    "minisblack-1c-16b.tiff",
    "minisblack-1c-8b.pgm",
    "minisblack-1c-8b.tiff",
    "minisblack-2c-8b-alpha.tiff",
    "miniswhite-1c-1b.g3",
    "miniswhite-1c-1b.pbm",
    "miniswhite-1c-1b.tiff",
    "ojpeg_chewey_subsamp21_multi_strip.tiff",
    "ojpeg_single_strip_no_rowsperstrip.tiff",
    "ojpeg_zackthecat_subsamp22_single_strip.tiff",
    "palette-1c-1b.tiff",
    "palette-1c-4b.tiff",
    "palette-1c-8b.tiff",
    "quad-lzw-compat.tiff",
    "quad-tile.jpg.tiff",
    "rgb-3c-16b.ppm",
    "rgb-3c-16b.tiff",
    "rgb-3c-8b.ppm",
    "rgb-3c-8b.tiff",
    "test_float64_predictor2_be_lzw.tif",
    "test_float64_predictor2_le_lzw.tif",
    "test_ifd_loop_subifd.tif",
    "test_ifd_loop_to_first.tif",
    "test_ifd_loop_to_self.tif",
    "test_two_ifds.tif",
    "testfax3_bug_513.tiff",
    "testfax4.tiff",
    "tiff_with_subifd_chain.tif",
    "webp_lossless_rgba_alpha_fully_opaque.tif",
]

# name -> (kind, url(s), prefix inside the archive, include patterns)
SOURCES = {
    "bmpsuite": (
        "zip",
        "https://entropymine.com/jason/bmpsuite/releases/bmpsuite-2.8.zip",
        "bmpsuite-2.8/",
        ["g/*.bmp", "q/*.bmp", "b/*.bmp", "html/*.png", "html/bmpsuite.html", "data/*.tif", "data/*.g3", "COPYING.txt"],
    ),
    "libtiff-pics": (
        "tar",
        "https://download.osgeo.org/libtiff/pics-3.8.0.tar.gz",
        "libtiffpic/",
        ["*.tif", "*.g3", "README", "depth/*.tif", "depth/README.txt", "depth/summary.txt"],
    ),
    "libtiff-test": (
        "files",
        {f: f"https://gitlab.com/libtiff/libtiff/-/raw/{LIBTIFF_TAG}/test/images/{f}" for f in LIBTIFF_TEST_FILES},
        "",
        ["*"],
    ),
    "gif-suite": (
        "tar",
        f"https://codeload.github.com/robert-ancell/pygif/tar.gz/{PYGIF_COMMIT}",
        f"pygif-{PYGIF_COMMIT}/test-suite/",
        ["*.gif", "*.conf", "*.rgba", "TESTS", "README.md", "*.icc", "*.xmp"],
    ),
}


def download(url):
    req = urllib.request.Request(url, headers={"User-Agent": "rivet-imagecodecs corpus fetch"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return r.read()


def members(name):
    """Yield (relative path, bytes) for every included file of a corpus."""
    kind, url, prefix, include = SOURCES[name]
    wanted = lambda rel: any(fnmatch.fnmatch(rel, p) for p in include) and "/CVS/" not in "/" + rel
    if kind == "files":
        for rel, u in url.items():
            yield rel, download(u)
        return
    blob = download(url)
    if kind == "zip":
        with zipfile.ZipFile(io.BytesIO(blob)) as z:
            for info in z.infolist():
                if info.is_dir() or not info.filename.startswith(prefix):
                    continue
                rel = info.filename[len(prefix):]
                if wanted(rel):
                    yield rel, z.read(info)
    else:
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:*") as t:
            for m in t.getmembers():
                if not m.isfile() or not m.name.startswith(prefix):
                    continue
                rel = m.name[len(prefix):]
                if wanted(rel):
                    yield rel, t.extractfile(m).read()


def load_manifest():
    entries = {}
    if os.path.exists(MANIFEST):
        with open(MANIFEST) as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#"):
                    digest, path = line.split(None, 1)
                    entries[path] = digest
    return entries


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    update = "--update-manifest" in sys.argv
    dest = os.path.abspath(args[0] if args else os.path.join(HERE, "..", "corpora"))
    manifest = {} if update else load_manifest()
    if not update and not manifest:
        sys.exit(f"no manifest at {MANIFEST}")
    seen = {}
    bad = []
    for name in SOURCES:
        print(f"fetching {name} ...", flush=True)
        count = 0
        for rel, data in members(name):
            path = f"{name}/{rel}"
            digest = hashlib.sha256(data).hexdigest()
            if update:
                seen[path] = digest
            elif path not in manifest:
                continue
            elif manifest[path] != digest:
                bad.append(path)
                continue
            out = os.path.join(dest, name, *rel.split("/"))
            os.makedirs(os.path.dirname(out), exist_ok=True)
            with open(out, "wb") as f:
                f.write(data)
            seen[path] = digest
            count += 1
        print(f"  {count} files")
    if update:
        with open(MANIFEST, "w", newline="\n") as f:
            f.write("# SHA-256 of every corpus file the tests use; written by fetch_corpora.py --update-manifest\n")
            for path in sorted(seen):
                f.write(f"{seen[path]}  {path}\n")
        print(f"wrote {len(seen)} entries to {MANIFEST}")
        return
    missing = sorted(set(manifest) - set(seen))
    if bad or missing:
        for p in bad:
            print(f"checksum mismatch: {p}", file=sys.stderr)
        for p in missing:
            print(f"missing: {p}", file=sys.stderr)
        sys.exit(1)
    print(f"{len(seen)} files verified into {dest}")


if __name__ == "__main__":
    main()
