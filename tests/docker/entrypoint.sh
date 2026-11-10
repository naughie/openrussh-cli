#!/bin/sh
set -eu

/usr/sbin/sshd -e

exec runuser -u alice -- "$@"
