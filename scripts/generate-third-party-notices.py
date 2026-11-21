#!/usr/bin/env python3
"""Generates THIRD_PARTY_NOTICES.txt for Foundation Studio Windows release bundles."""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import pathlib
import subprocess
import sys


LICENSE_PATTERNS = (
    "LICENSE*",
    "LICENCE*",
    "COPYING*",
    "NOTICE*",
    "COPYRIGHT*",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate THIRD_PARTY_NOTICES.txt from cargo metadata."
    )
    parser.add_argument(
        "--manifest",
        default="Cargo.toml",
        help="Path to Foundation-Studio Cargo.toml manifest.",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Output path for THIRD_PARTY_NOTICES.txt.",
    )
    parser.add_argument(
        "--target",
        default="x86_64-pc-windows-msvc",
        help="Target triple passed to cargo metadata --filter-platform.",
    )
    return parser.parse_args()


def load_metadata(manifest: pathlib.Path, target: str) -> dict:
    cmd = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--locked",
        "--filter-platform",
        target,
        "--manifest-path",
        str(manifest),
    ]
    out = subprocess.check_output(cmd, text=True, encoding="utf-8")
    return json.loads(out)


def collect_reachable_packages(meta: dict) -> tuple[list[dict], list[dict]]:
    pkg_by_id = {pkg["id"]: pkg for pkg in meta["packages"]}
    node_by_id = {node["id"]: node for node in meta["resolve"]["nodes"]}
    root_id = (meta.get("resolve") or {}).get("root")
    seeds = [root_id] if root_id else list(meta["workspace_members"])

    visited: set[str] = set()
    queue = collections.deque(seeds)
    while queue:
        cur = queue.popleft()
        if not cur or cur in visited:
            continue
        visited.add(cur)
        node = node_by_id.get(cur)
        if not node:
            continue
        for dep in node.get("deps", []):
            dep_kinds = dep.get("dep_kinds") or []
            is_non_dev = (not dep_kinds) or any(
                dk.get("kind") in (None, "build") for dk in dep_kinds
            )
            if not is_non_dev:
                continue
            dep_id = dep["pkg"]
            if dep_id not in visited:
                queue.append(dep_id)

    own_names = {"foundation-studio", "rmp-bridge-relay"}
    first_party = sorted(
        (pkg_by_id[pid] for pid in visited if pkg_by_id[pid]["name"] in own_names),
        key=lambda p: p["name"].lower(),
    )
    third_party = sorted(
        (pkg_by_id[pid] for pid in visited if pkg_by_id[pid]["name"] not in own_names),
        key=lambda p: (p["name"].lower(), p["version"]),
    )
    return first_party, third_party


def package_url(pkg: dict) -> str:
    repo = (pkg.get("repository") or "").strip()
    if repo:
        return repo
    homepage = (pkg.get("homepage") or "").strip()
    if homepage:
        return homepage
    src = (pkg.get("source") or "").strip()
    if src.startswith("git+"):
        return src[4:].split("?")[0].split("#")[0]
    if "crates.io-index" in src:
        return f"https://crates.io/crates/{pkg['name']}"
    return "-"


def find_notice_files(pkg_dir: pathlib.Path) -> list[pathlib.Path]:
    found: list[pathlib.Path] = []
    seen: set[pathlib.Path] = set()
    for pat in LICENSE_PATTERNS:
        for candidate in sorted(pkg_dir.glob(pat)):
            if candidate.is_file() and candidate not in seen:
                seen.add(candidate)
                found.append(candidate)
    licenses_subdir = pkg_dir / "LICENSES"
    if licenses_subdir.is_dir():
        for candidate in sorted(licenses_subdir.iterdir()):
            if candidate.is_file() and candidate not in seen:
                seen.add(candidate)
                found.append(candidate)
    return found


def read_text_clean(path: pathlib.Path) -> str:
    raw = path.read_text(encoding="utf-8", errors="replace")
    lines = [line.rstrip() for line in raw.replace("\r\n", "\n").replace("\r", "\n").split("\n")]
    return "\n".join(lines).strip()


def format_notices(
    first_party: list[dict], third_party: list[dict], target: str
) -> str:
    lines: list[str] = []
    lines.append("================================================================================")
    lines.append("Foundation Studio — Third-Party Software & External Component Notices")
    lines.append("================================================================================")
    lines.append("")
    lines.append("Project & Linked Companion Licenses:")
    lines.append("  - Foundation Studio: GNU General Public License v3.0 or later (GPL-3.0-or-later)")
    lines.append("    Source: https://github.com/bloooowfish/Foundation-Studio")
    lines.append("  - rmp-bridge-relay (rmp-bridge): GPL-3.0-or-later")
    lines.append("    Source: https://github.com/bloooowfish/rmp-bridge")
    lines.append("  - physis (by Joshua Goins / redstrate): GPL-3.0-or-later")
    lines.append("    Source: https://github.com/redstrate/physis")
    lines.append(f"  - Target platform: {target}")
    lines.append("")

    mpl_pkgs = [p for p in third_party if "MPL-2.0" in (p.get("license") or "")]
    lines.append("--------------------------------------------------------------------------------")
    lines.append("MPL-2.0 Source Availability Notice (MPL-2.0 Section 3.3)")
    lines.append("--------------------------------------------------------------------------------")
    lines.append("Under MPL-2.0 Section 3.3, unmodified MPL-2.0 components combined into this GPL-3.0")
    lines.append("Larger Work remain available under MPL-2.0 from their upstream repositories:")
    for p in mpl_pkgs:
        lines.append(f"  - {p['name']} {p['version']} ({p.get('license')}): {package_url(p)}")
    lines.append("")

    lines.append("--------------------------------------------------------------------------------")
    lines.append("Runtime-Downloaded External Components (NOT Bundled in This Archive)")
    lines.append("--------------------------------------------------------------------------------")
    lines.append("Foundation Studio can download or manage optional/external components directly from")
    lines.append("their upstream repositories at runtime upon user action. None of these are bundled:")
    lines.append("  - Foundation Server (bloooowfish/Foundation)")
    lines.append("    License: GNU Affero General Public License v3.0 (AGPL-3.0)")
    lines.append("    Launched as a separate external process and accessed over HTTP/SSE.")
    lines.append("    URL: https://github.com/bloooowfish/Foundation")
    lines.append("  - rmp-bridge Scripts & Lua Mods (bloooowfish/rmp-bridge)")
    lines.append("    License: GPL-3.0-or-later")
    lines.append("    URL: https://github.com/bloooowfish/rmp-bridge")
    lines.append("  - RE-UE4SS v4.0.0 (UE4SS-RE/RE-UE4SS)")
    lines.append("    License: MIT License (Copyright (c) 2022 Narknon & contributors)")
    lines.append("    Downloaded from official GitHub Releases with SHA-256 verification.")
    lines.append("    URL: https://github.com/UE4SS-RE/RE-UE4SS")
    lines.append("  - Oodle Network DLL (WorkingRobot/OodleUE)")
    lines.append("    NOTICE: Third-party unofficial build mirror — NOT an official Epic Games or")
    lines.append("    RAD Game Tools release. Use of Oodle binaries is subject to the Unreal Engine EULA")
    lines.append("    (https://www.unrealengine.com/eula/unreal). Users may alternatively place their own")
    lines.append("    locally obtained oodle-network-shared.dll without using auto-download.")
    lines.append("    URL: https://github.com/WorkingRobot/OodleUE")
    lines.append("  - ReMakePlace (RemakePlace/app)")
    lines.append("    NOTICE: Independent third-party closed-source application (no open-source license;")
    lines.append("    All Rights Reserved by its authors). Downloaded directly from RemakePlace/app")
    lines.append("    releases only when explicitly requested by the user.")
    lines.append("    URL: https://github.com/RemakePlace/app")
    lines.append("  - FINAL FANTASY XIV Client Data (Square Enix Co., Ltd.)")
    lines.append("    Studio reads SqPack Item/Stain tables directly from user-supplied local client")
    lines.append("    folders for on-device display only; no game data is redistributed.")
    lines.append("")

    lines.append("================================================================================")
    lines.append(f"First-Party / Companion Workspace Crates ({len(first_party)})")
    lines.append("================================================================================")
    for p in first_party:
        lines.append(f"  - {p['name']} {p['version']} ({p.get('license') or 'GPL-3.0-or-later'})")
    lines.append("")

    lines.append("================================================================================")
    lines.append(f"Resolved Third-Party Rust Crates ({len(third_party)})")
    lines.append("================================================================================")
    for p in third_party:
        lic = p.get("license") or "See crate notice/repository"
        lines.append(f"  - {p['name']} {p['version']} — {lic} — {package_url(p)}")
    lines.append("")

    groups: dict[str, dict] = {}
    group_order: list[str] = []
    no_license_files: list[str] = []

    for p in third_party:
        pkg_dir = pathlib.Path(p["manifest_path"]).parent
        notice_files = find_notice_files(pkg_dir)
        label = f"{p['name']} {p['version']}"
        if not notice_files:
            no_license_files.append(f"{label} ({p.get('license') or 'unspecified'}): {package_url(p)}")
            continue
        for nf in notice_files:
            text = read_text_clean(nf)
            if not text:
                continue
            digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            if digest not in groups:
                groups[digest] = {
                    "file": nf.name,
                    "license": p.get("license") or "-",
                    "packages": [],
                    "text": text,
                }
                group_order.append(digest)
            if label not in groups[digest]["packages"]:
                groups[digest]["packages"].append(label)

    if no_license_files:
        lines.append("--------------------------------------------------------------------------------")
        lines.append("Crates Without On-Disk LICENSE/NOTICE File (see upstream repository)")
        lines.append("--------------------------------------------------------------------------------")
        for item in no_license_files:
            lines.append(f"  - {item}")
        lines.append("")

    lines.append("================================================================================")
    lines.append(f"Verbatim License & Notice Texts ({len(group_order)} unique texts)")
    lines.append("================================================================================")
    lines.append("")

    for idx, digest in enumerate(group_order, 1):
        grp = groups[digest]
        pkg_list = ", ".join(grp["packages"])
        lines.append(f"--- Notice #{idx} ({grp['file']} | {grp['license']}) ---")
        lines.append(f"Packages: {pkg_list}")
        lines.append("")
        lines.append(grp["text"])
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    args = parse_args()
    manifest = pathlib.Path(args.manifest).resolve()
    if not manifest.is_file():
        print(f"Manifest not found: {manifest}", file=sys.stderr)
        return 1

    meta = load_metadata(manifest, args.target)
    first_party, third_party = collect_reachable_packages(meta)
    output_text = format_notices(first_party, third_party, args.target)

    out_path = pathlib.Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(output_text, encoding="utf-8", newline="\n")
    print(
        f"Generated {out_path} ({len(first_party)} first-party, "
        f"{len(third_party)} third-party crates, {len(output_text)} bytes)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
