default:
    @just --list

build:
    cargo build

install:
    cargo build --release
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install.ps1