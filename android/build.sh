#!/usr/bin/env bash
set -euo pipefail

android_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(dirname "$android_dir")
cache_dir=$android_dir/.cache
readonly android_dir repo_dir cache_dir

readonly image=amux-android-build
readonly image_label=io.github.blendonl.amux.dockerfile
readonly smoke_image=termux/termux-docker:x86_64
readonly jni_libs=android/app/src/main/jniLibs
readonly apk=app/build/outputs/apk/debug/app-debug.apk
readonly min_sdk=29
readonly abis=(arm64-v8a x86_64)

usage() {
    echo "usage: $0 image|binary|smoke|apk|all" >&2
    exit 2
}

die() {
    echo "$0: $*" >&2
    exit 1
}

dockerfile_hash() {
    git hash-object "$android_dir/docker/Dockerfile"
}

build_image() {
    local wanted built
    wanted=$(dockerfile_hash)
    built=$(docker image inspect --format "{{ index .Config.Labels \"$image_label\" }}" "$image" 2>/dev/null || true)
    if [[ "$built" == "$wanted" ]]; then
        echo "$image is up to date"
        return
    fi
    docker build --label "$image_label=$wanted" --tag "$image" "$android_dir/docker"
}

run_in_image() {
    local task=$1
    mkdir -p "$cache_dir/cargo" "$cache_dir/target" "$cache_dir/home" "$cache_dir/gradle" "$cache_dir/android"
    docker run --rm --init \
        --user "$(id -u):$(id -g)" \
        --volume "$repo_dir:/work" \
        --volume "$cache_dir:/cache" \
        --workdir /work \
        --env HOME=/cache/home \
        --env CARGO_HOME=/cache/cargo \
        --env CARGO_TARGET_DIR=/cache/target \
        --env GRADLE_USER_HOME=/cache/gradle \
        --env ANDROID_USER_HOME=/cache/android \
        "$image" \
        bash -c "set -euo pipefail
            $(declare -p jni_libs apk min_sdk abis)
            $(declare -f rust_target "$task")
            $task"
}

rust_target() {
    case "$1" in
        arm64-v8a) echo aarch64-linux-android ;;
        x86_64) echo x86_64-linux-android ;;
        *) echo "no rust target for abi $1" >&2 && return 1 ;;
    esac
}

cross_compile() {
    local targets=() abi
    for abi in "${abis[@]}"; do
        targets+=(-t "$abi")
    done
    cargo ndk --platform "$min_sdk" "${targets[@]}" build --release --locked --bin amux

    local strip=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip
    for abi in "${abis[@]}"; do
        mkdir -p "$jni_libs/$abi"
        "$strip" -o "$jni_libs/$abi/libamux.so" "$CARGO_TARGET_DIR/$(rust_target "$abi")/release/amux"
    done
    ls -l "$jni_libs"/*/libamux.so
}

build_binary() {
    run_in_image cross_compile
}

require_binaries() {
    local abi
    for abi in "${abis[@]}"; do
        [[ -f "$repo_dir/$jni_libs/$abi/libamux.so" ]] || die "$jni_libs/$abi/libamux.so is missing, run $0 binary first"
    done
}

build_apk() {
    cd android
    ./gradlew --no-daemon assembleDebug testDebugUnitTest lintDebug

    printf '\n$ ls -l %s\n' "$apk"
    ls -l "$apk"
    printf '\n$ unzip -l %s lib/*\n' "$apk"
    unzip -l "$apk" 'lib/*'
    printf '\n$ aapt2 dump badging %s\n' "$apk"
    aapt2 dump badging "$apk" | grep -E "^(package|minSdkVersion|targetSdkVersion|uses-permission|application-label|native-code)"
}

smoke_script() {
    cat <<'SMOKE'
set -eu
app=$HOME/io.github.blendonl.amux
files=$app/files
cache=$app/cache
mkdir -p "$files/home" "$files/config" "$files/state" "$files/bin" "$cache"
ln -s /smoke/lib/libamux.so "$files/bin/amux"

in_app() {
    env -i \
        HOME="$files/home" \
        XDG_CONFIG_HOME="$files/config" \
        XDG_STATE_HOME="$files/state" \
        TMPDIR="$cache" \
        SHELL=/system/bin/sh \
        PATH="$files/bin:/system/bin" \
        LANG=C.UTF-8 \
        "$@"
}

step() {
    printf '\n$ %s\n' "$*"
    in_app "$@"
}

server_failed() {
    echo "amux server $1, its log:" >&2
    cat "$app/server.log" >&2
    exit 1
}

step amux --version
step amux config check

printf '\n$ amux server &\n'
in_app amux server > "$app/server.log" 2>&1 &
server=$!

tries=0
until in_app amux ls > /dev/null 2>&1; do
    kill -0 "$server" 2> /dev/null || server_failed "exited before it answered"
    tries=$((tries + 1))
    [ "$tries" -lt 100 ] || server_failed "did not answer within 10 seconds"
    sleep 0.1
done

step amux ls
step amux kill-server

status=0
wait "$server" || status=$?
[ "$status" -eq 0 ] || server_failed "exited with status $status"
printf '\n$ cat server.log\n'
cat "$app/server.log"
SMOKE
}

smoke() {
    local lib_dir=$repo_dir/$jni_libs/x86_64
    [[ -f "$lib_dir/libamux.so" ]] || die "$lib_dir/libamux.so is missing, run $0 binary first"
    docker run --rm \
        --user 0 \
        --entrypoint /system/bin/sh \
        --volume "$lib_dir:/smoke/lib:ro" \
        "$smoke_image" \
        -c 'ln -s /system/bin /bin && exec /entrypoint.sh "$@"' sh \
        /system/bin/sh -c "$(smoke_script)" \
        || die "smoke test failed"
    printf '\nsmoke test passed\n'
}

main() {
    [[ $# -eq 1 ]] || usage
    case "$1" in
        image)
            build_image
            ;;
        binary)
            build_image
            build_binary
            ;;
        smoke)
            smoke
            ;;
        apk)
            build_image
            require_binaries
            run_in_image build_apk
            ;;
        all)
            build_image
            build_binary
            smoke
            run_in_image build_apk
            ;;
        *)
            usage
            ;;
    esac
}

main "$@"
