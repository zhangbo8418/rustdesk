#!/usr/bin/env bash
cd $(dirname $(dirname $(which flutter)))
git apply ~/rustdesk/.github/patches/flutter_3.13.9_dropdown_menu_enableFilter.diff
cargo build --features flutter,hwcodec --release --target aarch64-apple-ios --lib
