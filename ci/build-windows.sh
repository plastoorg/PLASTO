#!/usr/bin/env bash
# Кросс-сборка plasto.exe (x86_64-pc-windows-msvc) на Linux — в том числе на
# Raspberry Pi (aarch64). Тот же MSVC ABI, что у локальной сборки на Windows:
# CRT и Windows SDK скачивает cargo-xwin, линкует lld-link, иконку и манифест
# вшивает llvm-rc.
#
# Нужно на машине: rustup, clang, lld, llvm (Debian/Raspberry Pi OS:
# `sudo apt install clang lld llvm`). Тестам под Linux ещё нужен OpenSSL
# для native-tls: `sudo apt install libssl-dev pkg-config`.
#
# Результат: dist/PLASTO-<версия>.exe и dist/SHA256SUMS-<версия>.txt.
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=x86_64-pc-windows-msvc
TOOLCHAIN=${TOOLCHAIN:-stable}

for tool in clang lld-link llvm-rc; do
  command -v "$tool" >/dev/null || { echo "нет $tool: sudo apt install clang lld llvm" >&2; exit 1; }
done

# Toolchain машины по умолчанию не трогаем: ставим нужный рядом и зовём явно.
rustup toolchain install "$TOOLCHAIN" --profile minimal --target "$TARGET" --no-self-update

# cargo-xwin — одна сборка на машину, дальше берётся из ~/.cargo/bin.
if ! command -v cargo-xwin >/dev/null; then
  cargo "+$TOOLCHAIN" install cargo-xwin --locked
fi

cargo "+$TOOLCHAIN" xwin build --release --locked --target "$TARGET"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
name="PLASTO-$version.exe"
rm -rf dist && mkdir dist
cp "${CARGO_TARGET_DIR:-target}/$TARGET/release/plasto.exe" "dist/$name"
(cd dist && sha256sum "$name" > "SHA256SUMS-$version.txt" && cat "SHA256SUMS-$version.txt")
