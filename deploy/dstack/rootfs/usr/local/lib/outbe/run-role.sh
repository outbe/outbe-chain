#!/bin/bash
# Run one launch script of the provisioned validator bundle.
set -euo pipefail
role=${1:?role required}
# shellcheck disable=SC1091
. /run/outbe/env
exec "$OUTBE_BASE/validator-$OUTBE_VALIDATOR_INDEX/run-$role.sh"
