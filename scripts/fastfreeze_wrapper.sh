#!/bin/sh
set -e

FF_DIR=$(dirname -- "$(readlink -f -- "$0")")

# Pass the original PATH and LD_LIBRARY_PATH down to the application
export FF_APP_PATH=$PATH
export FF_APP_LD_LIBRARY_PATH=$LD_LIBRARY_PATH

# Override the PATH and LD_LIBRARY_PATH that fastfreeze should use
export LD_LIBRARY_PATH=$FF_DIR/lib:$LD_LIBRARY_PATH
export PATH=$FF_DIR:$PATH

# You may set the following environment variables
# FF_APP_VIRT_CPUID_MASK     The CPUID mask to use. See libvirtcpuid documentation for more details
# FF_APP_INJECT_<VAR_NAME>   Additional environment variables to inject to the application and its children.
#                            For example, FF_APP_INJECT_LD_PRELOAD=/opt/lib/libx.so
# FF_METRICS_RECORDER        When specified, FastFreeze invokes the specified program to report metrics.
#                            The metrics are formatted in JSON and passed as first argument
# CRIU_OPTS                  Additional arguments to pass to CRIU, whitespace separated
# S3_CMD                     Command to access AWS S3. Defaults to 'aws s3'
# GS_CMD                     Command to access Google Storage. Defaults to 'gsutil'

exec $FF_DIR/fastfreeze "$@"
