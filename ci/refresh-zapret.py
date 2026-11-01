#!/usr/bin/env python3
"""Встроенная в PLASTO сборка zapret: свежий релиз Flowseal/zapret-discord-youtube.

Без ключей — только проверка: печатает `changed=true|false` и `zapret=<тег>`
(строки для $GITHUB_OUTPUT). С `--apply` новый релиз скачивается, сверяется
с размером и SHA-256 от GitHub и раскладывается в assets/zapret без того, что
PLASTO не запускает; версия записывается в assets/zapret.json. С `--bump`
вдобавок поднимается версия PLASTO (1.0.0-beta-2 → 1.0.0-beta-3) и печатается
`version=<новая>`.

Только стандартная библиотека: скрипт работает на раннере CI (Raspberry Pi).
"""
import hashlib
import io
import json
import os
import re
import shutil
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
BUNDLE = ASSETS / "zapret"
META = ASSETS / "zapret.json"
API = "https://api.github.com/repos/Flowseal/zapret-discord-youtube/releases/latest"
DOWNLOAD = "https://github.com/Flowseal/zapret-discord-youtube/releases/download/"
MAX_ARCHIVE = 100 * 1024 * 1024

# Что из сборки Flowseal PLASTO не нужно: меню и установщик службы (у PLASTO
# свои), их тест стратегий и автообновление, запасной список IPSet, который
# включает только переключатель в service.bat.
SKIP_FILES = {"service.bat", "lists/ipset-all.txt.backup"}
SKIP_DIRS = {"utils"}


def request(url):
    req = urllib.request.Request(url, headers={"User-Agent": "PLASTO-ci", "Accept": "application/vnd.github+json"})
    token = os.environ.get("GH_TOKEN")
    if token and url.startswith("https://api.github.com/"):
        req.add_header("Authorization", f"Bearer {token}")
    return urllib.request.urlopen(req, timeout=60)


def latest():
    release = json.load(request(API))
    tag = release["tag_name"]
    asset = next(a for a in release["assets"] if a["name"].lower().startswith("zapret-discord-youtube") and a["name"].lower().endswith(".zip"))
    if not asset["browser_download_url"].startswith(DOWNLOAD):
        raise SystemExit(f"неверный адрес архива: {asset['browser_download_url']}")
    digest = (asset.get("digest") or "").removeprefix("sha256:")
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise SystemExit("у GitHub нет контрольной суммы архива")
    return {"tag": tag, "name": asset["name"], "url": asset["browser_download_url"], "size": asset["size"], "sha256": digest, "published": release.get("published_at")}


def keep(rel: str) -> bool:
    parts = rel.split("/")
    return rel not in SKIP_FILES and parts[0] not in SKIP_DIRS


def apply(rel):
    data = request(rel["url"]).read(MAX_ARCHIVE + 1)
    if len(data) != rel["size"] or hashlib.sha256(data).hexdigest() != rel["sha256"]:
        raise SystemExit("размер или SHA-256 архива не совпадает с GitHub")
    archive = zipfile.ZipFile(io.BytesIO(data))
    names = [n for n in archive.namelist() if not n.endswith("/")]
    # Сборка бывает в корне архива или в одной папке внутри.
    prefix = next((n[: -len("bin/winws.exe")] for n in names if n.endswith("bin/winws.exe")), None)
    if prefix is None:
        raise SystemExit("в архиве нет bin/winws.exe")
    with tempfile.TemporaryDirectory(dir=ASSETS) as tmp:
        stage = Path(tmp) / "zapret"
        for name in names:
            if not name.startswith(prefix):
                continue
            rel_path = name[len(prefix):]
            if ".." in rel_path.split("/") or rel_path.startswith("/") or not keep(rel_path):
                continue
            dest = stage / rel_path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(archive.read(name))
        for need in ["bin/winws.exe", "bin/cygwin1.dll", "bin/WinDivert.dll", "bin/WinDivert64.sys", "general.bat"]:
            if not (stage / need).is_file():
                raise SystemExit(f"в сборке нет {need}")
        if BUNDLE.exists():
            shutil.rmtree(BUNDLE)
        shutil.move(str(stage), BUNDLE)
    META.write_text(json.dumps({k: rel[k] for k in ["tag", "name", "sha256", "size", "published"]}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def bump() -> str:
    """Следующая версия PLASTO: номер в конце предварительной сборки +1,
    у выпущенной — следующая правка с «-beta-1»."""
    toml = ROOT / "Cargo.toml"
    text = toml.read_text(encoding="utf-8")
    current = re.search(r'^version = "([^"]+)"', text, re.M).group(1)
    m = re.fullmatch(r"(.*\D)(\d+)", current)
    if "-" in current and m:
        new = f"{m.group(1)}{int(m.group(2)) + 1}"
    else:
        major, minor, patch = (current.split("-")[0].split(".") + ["0", "0"])[:3]
        new = f"{major}.{minor}.{int(patch) + 1}-beta-1"
    toml.write_text(text.replace(f'version = "{current}"', f'version = "{new}"', 1), encoding="utf-8")
    lock = ROOT / "Cargo.lock"
    ltext = lock.read_text(encoding="utf-8")
    ltext, n = re.subn(r'(name = "plasto"\nversion = ")' + re.escape(current) + '"', lambda x: x.group(1) + new + '"', ltext, count=1)
    if n != 1:
        raise SystemExit("в Cargo.lock не найдена версия plasto")
    lock.write_text(ltext, encoding="utf-8")
    return new


def main():
    rel = latest()
    have = json.loads(META.read_text(encoding="utf-8"))["tag"] if META.is_file() else None
    changed = have != rel["tag"]
    print(f"changed={'true' if changed else 'false'}")
    print(f"zapret={rel['tag']}")
    print(f"previous={have or ''}")
    if changed and "--apply" in sys.argv:
        apply(rel)
        if "--bump" in sys.argv:
            print(f"version={bump()}")


if __name__ == "__main__":
    main()
