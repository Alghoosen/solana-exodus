#!/usr/bin/env bash
set -e

cd "$(dirname "$0")/.."

source ci/_
source ci/rust-version.sh stable
source ci/rust-version.sh nightly
eval "$(ci/channel-info.sh)"

export RUST_BACKTRACE=1
export RUSTFLAGS="-D warnings"


# Only force up-to-date lock files on edge
if [[ $CI_BASE_BRANCH = "$EDGE_CHANNEL" ]]; then
  if _ scripts/cargo-for-all-lock-files.sh +"$rust_nightly" check --locked --all-targets; then
    true
  else
    check_status=$?
    echo "Some Cargo.lock is outdated; please update them as well"
    echo "protip: you can use ./scripts/cargo-for-all-lock-files.sh update ..."
    exit "$check_status"
  fi
else
  echo "Note: cargo-for-all-lock-files.sh skipped because $CI_BASE_BRANCH != $EDGE_CHANNEL"
fi

_ cargo +"$rust_stable" fmt --all -- --check

_ cargo +"$rust_stable" clippy --version
_ cargo +"$rust_stable" clippy --workspace -- --deny=warnings

_ ci/order-crates-for-publishing.py

{
  cd programs/bpf
  for project in rust/*/ ; do
    echo "+++ do_bpf_checks $project"
    (
      cd "$project"
      _ cargo +"$rust_stable" fmt -- --check
      _ cargo +"$rust_nightly" test
      _ cargo +"$rust_nightly" clippy --version
      _ cargo +"$rust_nightly" clippy -- --deny=warnings --allow=clippy::missing_safety_doc
    )
  done
}

echo --- ok
