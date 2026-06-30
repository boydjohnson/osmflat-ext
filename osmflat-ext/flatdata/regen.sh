#!/usr/bin/env bash
# Regenerate src/ext_generated.rs from the flatdata schema.
# Requires: pip install flatdata-generator
set -euo pipefail
cd "$(dirname "$0")/.."
flatdata-generator -s flatdata/ext.flatdata -g rust -O src/ext_generated.rs
echo "regenerated src/ext_generated.rs"
