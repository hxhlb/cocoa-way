#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <path-to-app-bundle>" >&2
    exit 1
fi

APP_DIR=$1
MAIN_BIN="${APP_DIR}/Contents/MacOS/cocoa-way"
FRAMEWORKS_DIR="${APP_DIR}/Contents/Frameworks"

if [[ ! -f "${MAIN_BIN}" ]]; then
    echo "error: missing app executable at ${MAIN_BIN}" >&2
    exit 1
fi

mkdir -p "${FRAMEWORKS_DIR}"

QUEUE=("${MAIN_BIN}")

is_bundle_candidate() {
    case "$1" in
        /System/*|/usr/lib/*|@rpath/*|@loader_path/*|@executable_path/*)
            return 1
            ;;
        *)
            return 0
            ;;
    esac
}

add_rpath_if_needed() {
    local file=$1
    local rpath=$2

    install_name_tool -add_rpath "${rpath}" "${file}" 2>/dev/null || true
}

queue_file() {
    local file=$1
    local queued

    for queued in "${QUEUE[@]}"; do
        if [[ "${queued}" == "${file}" ]]; then
            return
        fi
    done

    QUEUE+=("${file}")
}

copy_and_rewrite_dependency() {
    local current=$1
    local dependency=$2
    local basename
    local bundled_path
    local rewritten_path

    basename=$(basename "${dependency}")
    bundled_path="${FRAMEWORKS_DIR}/${basename}"

    if [[ ! -e "${bundled_path}" ]]; then
        cp "${dependency}" "${bundled_path}"
        chmod u+w "${bundled_path}"
        install_name_tool -id "@rpath/${basename}" "${bundled_path}"
        add_rpath_if_needed "${bundled_path}" "@loader_path"
        queue_file "${bundled_path}"
    fi

    if [[ "${current}" == "${MAIN_BIN}" ]]; then
        rewritten_path="@executable_path/../Frameworks/${basename}"
    else
        rewritten_path="@rpath/${basename}"
        add_rpath_if_needed "${current}" "@loader_path"
    fi

    install_name_tool -change "${dependency}" "${rewritten_path}" "${current}" 2>/dev/null || true
}

add_rpath_if_needed "${MAIN_BIN}" "@executable_path/../Frameworks"

index=0
while [[ ${index} -lt ${#QUEUE[@]} ]]; do
    current=${QUEUE[${index}]}
    index=$((index + 1))

    while IFS= read -r dependency; do
        [[ -n "${dependency}" ]] || continue

        if is_bundle_candidate "${dependency}"; then
            copy_and_rewrite_dependency "${current}" "${dependency}"
        fi
    done < <(otool -L "${current}" | tail -n +2 | awk '{print $1}')
done
