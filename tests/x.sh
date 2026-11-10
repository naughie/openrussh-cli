#!/bin/sh

set -e

__proj_id="openrussh-client/cli"
__openssh_version="1:10.0p1-7+deb13u4"
__apt_snapshot="20261005T000000Z"
__test_ids="cli stdio terminal signals host-key-prompt listeners"

__print_usage() {
    printf 'Usage: %s [test_id] [build|run]\n' "${__script}" >&2
    exit 1
}

__is_test_id() {
    local __id
    for __id in ${__test_ids}; do
        if [ "${__id}" = "$1" ]; then
            return 0
        fi
    done
    return 1
}

__find_root_dir() {
    case "${__script}" in
      */*) local __script_path="${__script}" ;;
        *) local __script_path=$(command -v "${__script}") ;;
    esac

    local __script_dir="$(cd "$(dirname "${__script_path}")" && pwd)"
    dirname "${__script_dir}"
}

__build_image() {
    docker build \
        --build-arg "SSH_TEST_ID=${__test_id}" \
        --build-arg "OPENSSH_VERSION=${__openssh_version}" \
        --build-arg "APT_SNAPSHOT=${__apt_snapshot}" \
        -t "${__docker_image}" -f "${__dockerfile}" "${__docker_ctx}"
}

__run_image() {
    if [ "${__tty}" = true ]; then
        docker run --rm -t "${__docker_image}"
    else
        docker run --rm -a stdout -a stderr "${__docker_image}"
    fi
}

__x_one() {
    printf "test %s\n" "${__test_id}"

    if [ -z "${__cmd}" ]; then
        __build_image && __run_image
    else
        case "${__cmd}" in
            build)
                __build_image
                ;;
            run)
                __run_image
                ;;
            *)
                __print_usage
                ;;
        esac
    fi
}

__x() {
    local __num_suites=0
    local __failed_suites=0
    local __failure_msg=""

    for __test_id in ${__test_ids}; do
        local __docker_image="${__proj_id}/tests/${__test_id}"

        set +e

        __num_suites=$((__num_suites + 1))

        if [ "${__tty}" = true ]; then
            __x_one </dev/tty >/dev/tty 2>&1
        else
            __x_one
        fi
        local __status=$?

        set -e

        if [ "${__status}" -ne 0 ]; then
            __failed_suites=$((__failed_suites + 1))
            __failure_msg="${__failure_msg}  ${__test_id} (exit ${__status})"
        fi
    done

    printf '\nSummary: %s total, %s passed, %s failed\n' \
        "${__num_suites}" "$((__num_suites - __failed_suites))" "${__failed_suites}" >&2

    if [ "${__failed_suites}" -ne 0 ]; then
        printf 'Failed suites:\n%s\n' "${__failure_msg}" >&2
    fi

    set -e
    test "${__failed_suites}" -eq 0
}


__script="$0"

__arg1="${1-}"
__arg2="${2-}"

__repo_root="$(__find_root_dir)"
__dockerfile="${__repo_root}/tests/Dockerfile"
__docker_ctx="${__repo_root}"

case "$#" in
    0)
        __single_test=false
        ;;
    1)
        case "${__arg1}" in
            run|build) __single_test=false ;;
                    *)
                        if __is_test_id "${__arg1}"; then
                            __single_test=true
                        else
                            __print_usage
                        fi
                        ;;
        esac
        ;;
    2)
        case "${__arg2}" in
            run|build)
                if __is_test_id "${__arg1}"; then
                    __single_test=true
                else
                    __print_usage
                fi
                ;;
            *)
                __print_usage
                ;;
        esac
        ;;
    *)
        __print_usage
        ;;
esac

if [ "${__single_test}" = true ]; then
    __test_ids="${__arg1}"
    __cmd="${__arg2}"
else
    __cmd="${__arg1}"
fi

__tty=false
if ( : </dev/tty >/dev/tty ) 2>/dev/null; then
    __tty=true
fi

__x
