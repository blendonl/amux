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
readonly assets=android/app/src/main/assets
readonly smoke_lib=/smoke/lib
readonly apk=app/build/outputs/apk/debug/app-debug.apk
readonly release_apk=app/build/outputs/apk/release/app-release.apk
readonly release_key_env=(AMUX_RELEASE_KEYSTORE AMUX_RELEASE_KEYSTORE_PASSWORD AMUX_RELEASE_KEY_ALIAS)
readonly min_sdk=29
readonly abis=(arm64-v8a x86_64)
readonly release_abi=arm64-v8a

readonly userland_dir=$android_dir/userland
readonly userland_out=$cache_dir/userland
readonly termux_packages=$cache_dir/termux-packages
readonly builder=amux-userland-builder
readonly userland_arches=(aarch64 x86_64)
readonly app_data_dir=/data/data/io.github.blendonl.amux
readonly userland_prune=(include 'lib/*.a' lib/pkgconfig lib/cmake share/aclocal libexec/installed-tests var/service)
readonly forbidden_libs='^lib(db|krb5|k5crypto|krb5support|gssapi|gssapi_krb5|com_err)[-.]'
readonly android_libs=(libc.so libdl.so libm.so liblog.so libandroid.so)

usage() {
    echo "usage: $0 image|binary|userland|package|smoke|apk|release-apk|all|userland-check|userland-inputs" >&2
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
    shift
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
        "$@" \
        "$image" \
        bash -c "set -euo pipefail
            $(declare -p jni_libs assets smoke_lib apk release_apk min_sdk abis release_abi userland_arches)
            $(declare -f rust_target userland_abi describe_apk "$task")
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
    cargo ndk --platform "$min_sdk" "${targets[@]}" build --profile dist --locked --bin amux

    local strip=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip
    for abi in "${abis[@]}"; do
        mkdir -p "$jni_libs/$abi"
        "$strip" -o "$jni_libs/$abi/libamux.so" "$CARGO_TARGET_DIR/$(rust_target "$abi")/dist/amux"
    done
    ls -l "$jni_libs"/*/libamux.so
}

build_binary() {
    run_in_image cross_compile
}

userland_abi() {
    case "$1" in
        aarch64) echo arm64-v8a ;;
        x86_64) echo x86_64 ;;
        *) echo "no abi for userland arch $1" >&2 && return 1 ;;
    esac
}

package_prefixes() {
    local arch
    for arch in "${userland_arches[@]}"; do
        python3 -B android/userland/package.py \
            --prefix "/cache/userland/$arch/prefix" \
            --abi "$(userland_abi "$arch")" \
            --jnilibs "$jni_libs" \
            --assets "$assets"
    done
}

package_userland() {
    local arch
    for arch in "${userland_arches[@]}"; do
        [[ -x "$userland_out/$arch/prefix/bin/zsh" ]] || die "$userland_out/$arch/prefix is missing, run $0 userland first"
    done
    run_in_image package_prefixes
    du -sh "$repo_dir/$assets"/userland/*.zip
}

require_binaries() {
    local abi
    for abi in "${abis[@]}"; do
        [[ -f "$repo_dir/$jni_libs/$abi/libamux.so" ]] || die "$jni_libs/$abi/libamux.so is missing, run $0 binary first"
    done
}

require_userland() {
    local abi executables
    for abi in "${abis[@]}"; do
        [[ -f "$repo_dir/$assets/userland/$abi.zip" ]] || die "$assets/userland/$abi.zip is missing, run $0 package first"
        executables=("$repo_dir/$jni_libs/$abi"/libu_*.so)
        [[ -f "${executables[0]}" ]] || die "$jni_libs/$abi has no libu_*.so, run $0 package first"
    done
}

describe_apk() {
    local file=$1 abi
    shift
    printf '\n$ ls -lh %s\n' "$file"
    ls -lh "$file"
    printf '\n$ unzip -l %s lib/*/libamux.so assets/userland/*\n' "$file"
    unzip -l "$file" 'lib/*/libamux.so' 'assets/userland/*'
    for abi in "$@"; do
        printf '\n$ unzip -Z1 %s lib/%s/libu_*.so | wc -l\n' "$file" "$abi"
        unzip -Z1 "$file" "lib/$abi/libu_*.so" | wc -l
    done
    printf '\n$ aapt2 dump badging %s\n' "$file"
    aapt2 dump badging "$file" | grep -E "^(package|minSdkVersion|targetSdkVersion|uses-permission|application-label|native-code)"
}

build_apk() {
    cd android
    ./gradlew --no-daemon assembleDebug testDebugUnitTest lintDebug
    describe_apk "$apk" "${abis[@]}"
}

build_release_apk() {
    cd android
    ./gradlew --no-daemon -Pamux.abis="$release_abi" assembleRelease testReleaseUnitTest lintRelease
    describe_apk "$release_apk" "$release_abi"
    printf '\n$ unzip -Z1 %s | grep x86_64\n' "$release_apk"
    if unzip -Z1 "$release_apk" | grep x86_64; then
        echo "$release_apk carries x86_64 files, the release is arm64-only" >&2
        exit 1
    fi
    printf '\n$ apksigner verify --print-certs %s\n' "$release_apk"
    apksigner verify --print-certs "$release_apk"
}

require_release_key() {
    local name
    for name in "${release_key_env[@]}"; do
        [[ -n "${!name:-}" ]] || die "$name is not set, see Releasing in README.md"
    done
    [[ -f "$AMUX_RELEASE_KEYSTORE" ]] || die "AMUX_RELEASE_KEYSTORE names $AMUX_RELEASE_KEYSTORE, which is not a file"
}

release_apk_in_image() {
    run_in_image build_release_apk \
        --volume "$(realpath "$AMUX_RELEASE_KEYSTORE"):/release.keystore:ro" \
        --env AMUX_RELEASE_KEYSTORE=/release.keystore \
        --env AMUX_RELEASE_KEYSTORE_PASSWORD \
        --env AMUX_RELEASE_KEY_ALIAS
}

prepare_smoke_files() {
    rm -rf /cache/smoke
    python3 -B -c 'import sys
from pathlib import Path
sys.path.insert(0, "android/userland")
import package
package.install_into(Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]))' \
        /cache/smoke/files/usr "$assets/userland/x86_64.zip" "$smoke_lib"
    mkdir -p /cache/smoke/files/home
    sed 's/{{host}}/smoke-phone/' "$assets/dotfiles/zshrc" > /cache/smoke/files/home/.zshrc
}

smoke_script() {
    printf 'app=%s\nlib=%s\nsdk=%s\n' "$app_data_dir" "$smoke_lib" "$min_sdk"
    cat <<'SMOKE'
set -eu
files=$app/files
cache=$app/cache
prefix=$files/usr
log=$app/server.log

in_fallback() {
    env -i \
        HOME="$files/home" \
        XDG_CONFIG_HOME="$files/config" \
        XDG_STATE_HOME="$files/state" \
        TMPDIR="$cache" \
        SHELL=/system/bin/sh \
        PATH="$files/bin:/system/bin" \
        LANG=C.UTF-8 \
        ANDROID_DATA="$ANDROID_DATA" \
        ANDROID_ROOT="$ANDROID_ROOT" \
        "$@"
}

in_app() {
    env -i \
        HOME="$files/home" \
        XDG_CONFIG_HOME="$files/config" \
        XDG_STATE_HOME="$files/state" \
        PREFIX="$prefix" \
        TERMUX__PREFIX="$prefix" \
        TERMUX_APP__DATA_DIR="$app" \
        TERMUX_APP__LEGACY_DATA_DIR="$app" \
        LD_PRELOAD="$prefix/lib/libtermux-exec-direct-ld-preload.so" \
        TERMUX_EXEC__SYSTEM_LINKER_EXEC__MODE=disable \
        ANDROID__BUILD_VERSION_SDK="$sdk" \
        TMPDIR="$prefix/tmp" \
        PATH="$prefix/bin:/system/bin" \
        SHELL="$prefix/bin/zsh" \
        LANG=en_US.UTF-8 \
        ANDROID_DATA="$ANDROID_DATA" \
        ANDROID_ROOT="$ANDROID_ROOT" \
        "$@"
}

step() {
    runner=$1
    shift
    printf '\n$ %s\n' "$*"
    "$runner" "$@"
}

server_failed() {
    echo "amux server $1, its log:" >&2
    cat "$log" >&2
    exit 1
}

start_server() {
    printf '\n$ amux server &\n'
    "$1" amux server > "$log" 2>&1 &
    server=$!
    tries=0
    until "$1" amux ls > /dev/null 2>&1; do
        kill -0 "$server" 2> /dev/null || server_failed "exited before it answered"
        tries=$((tries + 1))
        [ "$tries" -lt 100 ] || server_failed "did not answer within 10 seconds"
        sleep 0.1
    done
    step "$1" amux ls
}

stop_server() {
    step "$1" amux kill-server
    status=0
    wait "$server" || status=$?
    [ "$status" -eq 0 ] || server_failed "exited with status $status"
    printf '\n$ cat server.log\n'
    cat "$log"
}

check_pane_shell() {
    printf '\n$ amux new -s smoke, in a pty; in its pane: print the shell, then exit\n'
    in_app zsh -c '
        zmodload zsh/zpty
        zpty client "stty rows 24 cols 80; TERM=xterm-256color COLORTERM=truecolor exec amux new -s smoke"
        screen=
        deadline=$((SECONDS + 10))
        while [[ $screen != *smoke-phone* ]] && ((SECONDS < deadline)); do
            zpty -r -t client chunk && screen+=$chunk || sleep 0.1
        done
        [[ $screen == *smoke-phone* ]] || { print -r -- "the pane showed no zsh prompt: ${(q+)screen}" >&2; exit 1 }
        zpty -w client "print -r -- \"\$0 \$ZSH_VERSION in \$(readlink /proc/\$\$/exe), SHELL=\$SHELL TERM=\$TERM\" > \$TMPDIR/pane; exit"
        while [[ ! -s $TMPDIR/pane ]] && ((SECONDS < deadline + 10)); do
            sleep 0.1
        done
        zpty -d client
        [[ -s $TMPDIR/pane ]] || { print -r -- "the pane did not run the command" >&2; exit 1 }
        cat $TMPDIR/pane
    '
}

printf '\n# the /system/bin/sh fallback, with no userland\n'
mkdir -p "$files/config" "$files/state" "$files/bin" "$cache"
ln -s "$lib/libamux.so" "$files/bin/amux"
cd "$files/home"
step in_fallback amux --version
step in_fallback amux config check
start_server in_fallback
stop_server in_fallback

printf '\n# the userland, laid out as the app installs it\n'
rm -r "$files/bin"
mkdir -m 700 "$prefix/tmp"
ln -s ../../applib/libamux.so "$prefix/bin/amux"
cat > "$prefix/tmp/usr-bin-env" <<'SCRIPT'
#!/usr/bin/env sh
echo "$0 runs in $(readlink /proc/$$/exe)"
SCRIPT
cat > "$prefix/tmp/prefix-sh" <<SCRIPT
#!$prefix/bin/sh
echo "\$0 runs in \$(readlink /proc/\$\$/exe)"
SCRIPT
chmod 700 "$prefix/tmp/usr-bin-env" "$prefix/tmp/prefix-sh"

step in_app ls -l "$files/applib" "$prefix/bin/zsh" "$prefix/bin/amux"
step in_app zsh -c 'echo $ZSH_VERSION'
step in_app zsh -c '"$TMPDIR/usr-bin-env"'
step in_app zsh -c '"$TMPDIR/prefix-sh"'
printf '\n$ env -u LD_PRELOAD zsh -c "$TMPDIR/usr-bin-env", which must fail without termux-exec\n'
if in_app env -u LD_PRELOAD zsh -c '"$TMPDIR/usr-bin-env"'; then
    echo "the /usr/bin/env shebang ran without termux-exec, so the check above proves nothing" >&2
    exit 1
fi
step in_app zsh -c 'cd "$TMPDIR" && git init -q repo && cd repo && echo hello > README &&
    git add README && git -c user.name=amux -c user.email=amux@localhost commit -q -m first && git log --stat'
step in_app ssh -V
step in_app curl -V
step in_app amux --version
start_server in_app
check_pane_shell
stop_server in_app

printf '\nnote: Docker has no SELinux, so this does not prove that W^X lets panes exec through applib; check that on a phone\n'
SMOKE
}

smoke() {
    local lib_dir=$repo_dir/$jni_libs/x86_64
    [[ -f "$lib_dir/libamux.so" ]] || die "$lib_dir/libamux.so is missing, run $0 binary first"
    require_userland
    run_in_image prepare_smoke_files
    docker run --rm \
        --user 0 \
        --entrypoint /system/bin/sh \
        --volume "$lib_dir:$smoke_lib:ro" \
        --volume "$cache_dir/smoke/files:/smoke/files:ro" \
        "$smoke_image" \
        -c "ln -s /system/bin /bin
            mkdir -p $app_data_dir
            cp -a /smoke/files $app_data_dir/files
            chown -R system:system $app_data_dir
            exec /entrypoint.sh \"\$@\"" sh \
        /system/bin/sh -c "$(smoke_script)" \
        || die "smoke test failed"
    rm -rf "$cache_dir/smoke"
    printf '\nsmoke test passed\n'
}

pinned() {
    sed -n "s/^$1=//p" "$userland_dir/termux-packages.txt"
}

userland_build_inputs() {
    cat "$userland_dir/termux-packages.txt" "$userland_dir/packages.txt" "$userland_dir"/overlay/*.patch \
        | sha256sum | cut -d ' ' -f 1
}

userland_inputs() {
    {
        userland_build_inputs
        declare -p userland_prune
        declare -f assemble_prefix deb_field deb_depends link_alternatives prune_prefix
    } | sha256sum | cut -d ' ' -f 1
}

userland_is_current() {
    local arch
    [[ -f "$userland_out/INPUTS" && "$(< "$userland_out/INPUTS")" == "$1" ]] || return 1
    [[ -f "$userland_out/sources/termux-packages.txt" ]] || return 1
    for arch in "${userland_arches[@]}"; do
        [[ -x "$userland_out/$arch/prefix/bin/zsh" ]] || return 1
    done
}

checkout_termux_packages() {
    local commit
    commit=$(pinned commit)
    [[ -d "$termux_packages/.git" ]] || git init -q "$termux_packages"
    if ! git -C "$termux_packages" cat-file -e "$commit^{commit}" 2> /dev/null; then
        git -C "$termux_packages" fetch -q --depth 1 "$(pinned repo)" "$commit"
    fi
    git -C "$termux_packages" checkout -q --force --detach "$commit"
    git -C "$termux_packages" clean -q -fd
    git -C "$termux_packages" apply "$userland_dir"/overlay/*.patch
    echo "termux-packages is at $commit with $(ls "$userland_dir/overlay" | wc -l) overlay patches"
}

create_builder() {
    local uid gid
    uid=$(id -u)
    gid=$(id -g)
    mkdir -p "$userland_out"
    docker run --detach --init --tty \
        --name "$builder" \
        --volume "$termux_packages:/home/builder/termux-packages" \
        --volume "$userland_out:/userland" \
        --security-opt "seccomp=$termux_packages/scripts/profile.json" \
        "$(pinned image)" > /dev/null
    [[ "$uid:$gid" != 1001:1001 ]] || return 0
    echo "giving the builder user uid $uid and gid $gid, this copies its home and takes a few minutes"
    docker exec "$builder" sudo chown -R "$uid:$gid" /home/builder /data
    docker exec "$builder" sudo usermod -u "$uid" builder
    docker exec "$builder" sudo groupmod -g "$gid" builder
}

start_builder() {
    local running
    running=$(docker container inspect --format '{{ .Config.Image }}' "$builder" 2> /dev/null || true)
    if [[ -n "$running" && "$running" != "$(pinned image)" ]]; then
        echo "the builder runs $running, recreating it from $(pinned image)"
        docker rm -f "$builder" > /dev/null
    fi
    case "$(docker container inspect --format '{{ .State.Running }}' "$builder" 2> /dev/null || true)" in
        true) ;;
        false) docker start "$builder" > /dev/null ;;
        *) create_builder ;;
    esac
}

in_builder() {
    CONTAINER_NAME=$builder TERMUX_BUILDER_IMAGE_NAME=$(pinned image) \
        "$termux_packages/scripts/run-docker.sh" "$@"
}

in_builder_run() {
    local task=$1
    shift
    in_builder bash -c "set -euo pipefail
        $(declare -p app_data_dir userland_prune forbidden_libs android_libs)
        $(declare -f assemble_prefix deb_field deb_depends link_alternatives prune_prefix copy_upstream_sources \
            inspect_prefix check_needed report_prefix)
        $task \"\$@\"" "$task" "$@"
}

package_dirs() {
    local name subpackages
    while read -r name; do
        if [[ -f "$termux_packages/packages/$name/build.sh" ]]; then
            echo "$name"
            continue
        fi
        subpackages=("$termux_packages"/packages/*/"$name".subpackage.sh)
        [[ -f "${subpackages[0]}" ]] || die "$name in packages.txt is not a termux package or subpackage"
        basename "$(dirname "${subpackages[0]}")"
    done < "$userland_dir/packages.txt" | sort -u
}

build_packages() {
    local wanted stamp=$termux_packages/output/.inputs dirs arch
    wanted=$(userland_build_inputs)
    if [[ ! -f "$stamp" || "$(< "$stamp")" != "$wanted" ]]; then
        echo "userland inputs changed, cleaning the builder"
        in_builder ./clean.sh
        rm -rf "$termux_packages/output"
        mkdir -p "$termux_packages/output"
        echo "$wanted" > "$stamp"
    fi
    dirs=$(package_dirs)
    for arch in "${userland_arches[@]}"; do
        echo "building for $arch:" $dirs
        in_builder ./build-package.sh -a "$arch" $dirs
    done
}

deb_field() {
    dpkg-deb --info "$1" control | sed -n "s/^$2: //p"
}

deb_depends() {
    local deb=$1 field alternatives alternative
    for field in Pre-Depends Depends; do
        deb_field "$deb" "$field"
    done | tr ',' '\n' | sed -E 's/\([^)]*\)//g; s/[[:space:]]//g; /^$/d' |
        while IFS='|' read -ra alternatives; do
            for alternative in "${alternatives[@]}"; do
                if [[ -n "${deb_of[$alternative]:-}" ]]; then
                    echo "$alternative"
                    continue 2
                fi
            done
            echo "${alternatives[0]}"
        done
}

link_alternatives() {
    local prefix=$1 name priority group link path
    shift
    for name in "$@"; do
        dpkg-deb --info "${deb_of[$name]}" postinst 2> /dev/null || true
    done | sed -nE 's|.*--install "([^"]+)" "([^"]+)" "([^"]+)" ([0-9]+).*|\4 \2 \1 \3|p' |
        sort -k1,1nr -k2,2 | awk '!seen[$2]++' |
        while read -r priority group link path; do
            link=$prefix/${link#"$app_data_dir"/files/usr/}
            path=$prefix/${path#"$app_data_dir"/files/usr/}
            ln -sfn "$(realpath -m --relative-to="$(dirname "$link")" "$path")" "$link"
            echo "alternative $group: ${link#"$prefix"/} -> ${path#"$prefix"/} (priority $priority)"
        done
}

prune_prefix() {
    local prefix=$1 path
    for path in "${userland_prune[@]}"; do
        rm -rf "${prefix:?}"/$path
    done
}

assemble_prefix() {
    local arch=$1 deb name stray
    local out=/userland/$arch
    local staging=$out/staging
    local usr=$staging$app_data_dir/files/usr
    local -A deb_of=() seen=()
    local -a queue=("${@:2}") closure=()
    rm -rf "$out"
    mkdir -p "$staging"
    for deb in output/*_"$arch".deb output/*_all.deb; do
        if [[ -f "$deb" ]]; then
            deb_of[$(deb_field "$deb" Package)]=$deb
        fi
    done
    while (( ${#queue[@]} )); do
        name=${queue[0]}
        queue=("${queue[@]:1}")
        [[ -z "${seen[$name]:-}" ]] || continue
        seen[$name]=1
        deb=${deb_of[$name]:-}
        if [[ -z "$deb" ]]; then
            echo "no $arch package named $name in output/" >&2
            return 1
        fi
        closure+=("$name")
        mapfile -t -O "${#queue[@]}" queue < <(deb_depends "$deb")
    done
    for name in "${closure[@]}"; do
        dpkg-deb --fsys-tarfile "${deb_of[$name]}" | tar -x --preserve-permissions -C "$staging"
        echo "$name $(deb_field "${deb_of[$name]}" Version)"
    done | sort > "$out/packages.txt"
    stray=$(find "$staging" -path "$usr" -prune -o ! -type d -print)
    if [[ -n "$stray" ]]; then
        echo "$arch packages install files outside the prefix:" "$stray" >&2
        return 1
    fi
    mv "$usr" "$out/prefix"
    rm -rf "$staging"
    link_alternatives "$out/prefix" "${closure[@]}"
    prune_prefix "$out/prefix"
    echo "$arch prefix holds ${#closure[@]} packages:" "${closure[@]}"
}

copy_upstream_sources() {
    local cache pkg
    for cache in "$HOME"/.termux-build/*/cache; do
        pkg=$(basename "$(dirname "$cache")")
        [[ "$pkg" != _* ]] || continue
        mkdir -p "/userland/sources/upstream/$pkg"
        cp -a "$cache"/. "/userland/sources/upstream/$pkg/"
    done
}

collect_sources() {
    rm -rf "$userland_out/sources"
    in_builder_run copy_upstream_sources
    cp "$userland_dir/termux-packages.txt" "$userland_dir/packages.txt" "$android_dir/build.sh" "$userland_out/sources/"
    cp -R "$userland_dir/overlay" "$userland_out/sources/"
    echo "sources: $(du -sh "$userland_out/sources" | cut -f 1) in $userland_out/sources"
}

inspect_prefix() {
    local prefix=/userland/$1/prefix file kind needed
    printf '\177ELF' > /tmp/elf-magic
    find "$prefix" -type f -printf '%P\n' | LC_ALL=C sort | while IFS= read -r file; do
        cmp -s -n 4 "$prefix/$file" /tmp/elf-magic || continue
        needed=$(readelf -dW "$prefix/$file" | sed -nE 's/.*\(NEEDED\).*\[(.*)\]$/\1/p' | paste -sd , -)
        if readelf -lW "$prefix/$file" | grep -q 'Requesting program interpreter'; then
            kind=exec
        elif [[ -z "$needed" ]]; then
            kind=static
        else
            kind=lib
        fi
        echo "$kind $file ${needed:--}"
    done > "/userland/$1/elf.txt"
}

check_needed() {
    local arch=$1 prefix=/userland/$1/prefix kind file needed lib status=0
    while read -r kind file needed; do
        for lib in ${needed//,/ }; do
            if [[ "$lib" =~ $forbidden_libs ]]; then
                echo "$arch: $file needs $lib" >&2
                status=1
            elif [[ "$lib" != - && ! -e "$prefix/lib/$lib" && " ${android_libs[*]} " != *" $lib "* ]]; then
                echo "$arch: $file needs $lib, which is neither in the prefix nor in Android" >&2
                status=1
            fi
        done
    done < "/userland/$arch/elf.txt"
    [[ $status -eq 0 ]] || return 1
    echo "$arch: $(wc -l < "/userland/$arch/elf.txt") ELF files, none needs libdb, libkrb5, libgssapi or libcom_err, every NEEDED library resolves"
}

report_prefix() {
    local arch=$1 prefix=/userland/$1/prefix
    printf '\n%s prefix: %s MiB, %s files, %s symlinks, %s directories\n' "$arch" \
        "$(du -sb "$prefix" | awk '{ printf "%.1f", $1 / 1048576 }')" \
        "$(find "$prefix" -type f | wc -l)" "$(find "$prefix" -type l | wc -l)" "$(find "$prefix" -type d | wc -l)"
    awk '{ kinds[$1]++ } END { printf "ELF files: %d executables, %d shared libraries, %d with no NEEDED (static)\n", kinds["exec"], kinds["lib"], kinds["static"] }' \
        "/userland/$arch/elf.txt"
    echo "largest files:"
    find "$prefix" -type f -printf '%s %P\n' | sort -rn | awk 'NR <= 10 { printf "  %7.1f KiB  %s\n", $1 / 1024, $2 }'
}

report_userland() {
    local arch
    for arch in "${userland_arches[@]}"; do
        in_builder_run inspect_prefix "$arch"
        in_builder_run report_prefix "$arch"
    done
}

build_userland() {
    local wanted arch
    wanted=$(userland_inputs)
    if userland_is_current "$wanted"; then
        echo "userland is up to date ($wanted), skipping the build"
        return
    fi
    rm -f "$userland_out/INPUTS"
    checkout_termux_packages
    start_builder
    build_packages
    for arch in "${userland_arches[@]}"; do
        in_builder_run assemble_prefix "$arch" $(< "$userland_dir/packages.txt")
    done
    collect_sources
    report_userland
    echo "$wanted" > "$userland_out/INPUTS"
}

userland_check_script() {
    cat <<'CHECK'
set -eu
files=/data/data/io.github.blendonl.amux/files
prefix=$files/usr
mkdir -p "$files/home" "$prefix/tmp"

in_pane() {
    env -i \
        HOME="$files/home" \
        PREFIX="$prefix" \
        TMPDIR="$prefix/tmp" \
        PATH="$prefix/bin:/system/bin" \
        LANG=en_US.UTF-8 \
        "$@"
}

step() {
    printf '\n$ %s\n' "$*"
    in_pane "$@"
}

cat > "$prefix/tmp/shebang" <<SCRIPT
#!$prefix/bin/sh
echo "\$0 runs in \$(readlink /proc/\$\$/exe)"
SCRIPT
cat > "$prefix/tmp/usr-bin-env" <<'SCRIPT'
#!/usr/bin/env sh
echo "$0 runs in $(readlink /proc/$$/exe)"
SCRIPT
chmod 700 "$prefix/tmp/shebang" "$prefix/tmp/usr-bin-env"

step id
step zsh -c 'echo ok'
step zsh -l -c 'echo ok from a login shell'
step bash --version
step git --version
step zsh -c 'cd "$TMPDIR" && git init -q repo && cd repo && echo hello > README &&
    git add README && git -c user.name=amux -c user.email=amux@localhost commit -q -m first && git log --stat'
step ssh -V
step ssh-keygen -t ed25519 -N '' -C amux -f "$prefix/tmp/id_ed25519"
step curl -V
step nano --version
step less --version
step "$prefix/bin/ls" -l "$prefix/bin/zsh" "$prefix/bin/sh"
step "$prefix/bin/grep" --version
step "$prefix/bin/sed" --version
step zsh -c 'command -v ls grep sed && echo amux | grep -o mu | sed s/mu/MU/'
step "$prefix/tmp/shebang"
step env \
    LD_PRELOAD="$prefix/lib/libtermux-exec-direct-ld-preload.so" \
    TERMUX_EXEC__SYSTEM_LINKER_EXEC__MODE=disable \
    TERMUX__PREFIX="$prefix" \
    TERMUX_APP__DATA_DIR=/data/data/io.github.blendonl.amux \
    sh -c "$prefix/tmp/usr-bin-env"
CHECK
}

check_userland_runs() {
    docker run --rm \
        --user 0 \
        --entrypoint /system/bin/sh \
        --volume "$userland_out/x86_64/prefix:/userland/prefix:ro" \
        "$smoke_image" \
        -c 'ln -s /system/bin /bin
            mkdir -p /data/data/io.github.blendonl.amux/files
            cp -a /userland/prefix /data/data/io.github.blendonl.amux/files/usr
            chown -R system:system /data/data/io.github.blendonl.amux
            exec /entrypoint.sh "$@"' sh \
        /system/bin/sh -c "$(userland_check_script)" \
        || die "the x86_64 userland does not run in $smoke_image"
}

check_userland() {
    local arch
    for arch in "${userland_arches[@]}"; do
        [[ -x "$userland_out/$arch/prefix/bin/zsh" ]] || die "$userland_out/$arch/prefix is missing, run $0 userland first"
    done
    start_builder
    for arch in "${userland_arches[@]}"; do
        in_builder_run inspect_prefix "$arch"
        in_builder_run check_needed "$arch" || die "$arch has ELF files with unwanted or missing libraries"
    done
    check_userland_runs
    printf '\nuserland check passed\n'
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
            build_image
            smoke
            ;;
        userland)
            build_userland
            ;;
        package)
            build_image
            package_userland
            ;;
        apk)
            build_image
            require_binaries
            require_userland
            run_in_image build_apk
            ;;
        release-apk)
            require_release_key
            build_image
            require_binaries
            require_userland
            release_apk_in_image
            ;;
        all)
            build_image
            build_binary
            build_userland
            package_userland
            smoke
            run_in_image build_apk
            ;;
        userland-check)
            check_userland
            ;;
        userland-inputs)
            userland_inputs
            ;;
        *)
            usage
            ;;
    esac
}

main "$@"
