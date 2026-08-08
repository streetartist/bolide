#!/usr/bin/env python3
"""Export Bolide mastery book Markdown to PDF via pandoc + xelatex."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
MD = HERE / "bolide-from-zero-to-mastery.md"
PDF = HERE / "Bolide从入门到精通.pdf"


def main() -> int:
    if not MD.is_file():
        print(f"missing markdown: {MD}", file=sys.stderr)
        return 1

    pandoc = shutil.which("pandoc")
    if not pandoc:
        print("pandoc not found on PATH", file=sys.stderr)
        return 1

    cmd = [
        pandoc,
        str(MD),
        "-o",
        str(PDF),
        "--pdf-engine=xelatex",
        "-V",
        "CJKmainfont=Microsoft YaHei",
        "-V",
        "mainfont=Microsoft YaHei",
        "-V",
        "monofont=Consolas",
        "-V",
        "geometry:margin=2.2cm",
        "-V",
        "fontsize=11pt",
        "--toc",
        "--toc-depth=2",
        "-V",
        "colorlinks=true",
        "-V",
        "linkcolor=blue",
        "--highlight-style=tango",
        "-V",
        "documentclass=article",
        "--metadata",
        "title=Bolide 从入门到精通",
        "--metadata",
        "author=Bolide Team",
        "--metadata",
        "lang=zh-CN",
    ]

    print("running:", " ".join(cmd))
    # MiKTeX may need network for first-time package install
    proc = subprocess.run(cmd, cwd=str(ROOT))
    if proc.returncode != 0:
        print("pandoc failed", file=sys.stderr)
        return proc.returncode

    size = PDF.stat().st_size if PDF.is_file() else 0
    print(f"wrote {PDF} ({size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
