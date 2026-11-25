#!/usr/bin/env bash
# One-command Android build: Rust `libsegnale_debole.so` (arm64-v8a) via
# cargo-ndk, then the APK via the Gradle wrapper.
#
#   android/build-apk.sh            # release Rust lib + assembleDebug (default)
#   android/build-apk.sh --debug    # debug Rust lib   + assembleDebug
#   android/build-apk.sh --release  # release Rust lib + assembleRelease
#                                   # (signed with the debug key, sideload only)
#
# Needs: JDK 17, Android SDK (ANDROID_HOME, platform 35, build-tools), NDK
# (ANDROID_NDK_HOME or ANDROID_HOME/ndk/<ver>), rustup target
# aarch64-linux-android and `cargo install cargo-ndk`.
# Prints the APK path on success. Optional: OUT=<file> copies the APK there.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

rust_profile=release
gradle_task=assembleDebug
apk_variant=debug
for arg in "$@"; do
  case "$arg" in
    --debug) rust_profile=debug ;;
    --release) rust_profile=release; gradle_task=assembleRelease; apk_variant=release ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

: "${ANDROID_HOME:=${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"
export ANDROID_HOME
if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -n1 || true)"
fi
[[ -d "${ANDROID_NDK_HOME:-}" ]] || { echo "Android NDK not found (set ANDROID_NDK_HOME)" >&2; exit 1; }
export ANDROID_NDK_HOME
command -v cargo-ndk >/dev/null || { echo "cargo-ndk missing: cargo install cargo-ndk" >&2; exit 1; }

jni="$here/app/src/main/jniLibs"
rm -rf "$jni/arm64-v8a"
mkdir -p "$jni"

echo "==> cargo ndk ($rust_profile) -> $jni/arm64-v8a"
# -P 26 = minSdk: AAudio (cpal) only exists from API 26.
cargo_args=(ndk -t arm64-v8a -P 26 -o "$jni" build --lib)
[[ "$rust_profile" == release ]] && cargo_args+=(--release)
(cd "$root" && cargo "${cargo_args[@]}")

so="$jni/arm64-v8a/libsegnale_debole.so"
[[ -f "$so" ]] || { echo "missing $so" >&2; exit 1; }

# Package every non-system shared library the game links against. Today the
# graph is fully static (android-activity links c++_static, cpal uses AAudio),
# but if a dependency ever needs libc++_shared.so it must ship in the APK.
readelf_bin="$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-readelf 2>/dev/null | head -n1)"
if [[ -x "$readelf_bin" ]]; then
  needed="$("$readelf_bin" -d "$so" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')"
  echo "==> NEEDED: $(echo $needed)"
  if grep -qx 'libc++_shared.so' <<<"$needed"; then
    cp "$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so | head -n1)" "$jni/arm64-v8a/"
    echo "    packaged libc++_shared.so"
  fi
fi

echo "==> gradle $gradle_task"
(cd "$here" && ./gradlew --no-daemon "$gradle_task")

apk="$here/app/build/outputs/apk/$apk_variant/app-$apk_variant.apk"
[[ -f "$apk" ]] || { echo "missing $apk" >&2; exit 1; }
if [[ -n "${OUT:-}" ]]; then
  cp "$apk" "$OUT"
  apk="$OUT"
fi
echo "$apk"
