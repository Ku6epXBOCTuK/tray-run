default:
    @just --list

build:
    cargo build

install:
    cargo build --release
    cp target/release/tray-run.exe ~/bin/