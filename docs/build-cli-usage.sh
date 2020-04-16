#!/usr/bin/env bash
set -e

cd "$(dirname "$0")"

<<<<<<< HEAD
usage=$(cargo -q run -p solana-cli -- -C ~/.foo --help | sed 's|'"$HOME"'|~|g')
=======
usage=$(cargo +"$rust_stable" -q run -p solana-cli -- -C ~/.foo --help | sed -e 's|'"$HOME"'|~|g' -e 's/[[:space:]]\+$//')
>>>>>>> d567799d4... Use $rust_stable

out=${1:-src/cli/usage.md}

cat src/cli/.usage.md.header > "$out"

section() {
  declare mark=${2:-"###"}
  declare section=$1
  read -r name rest <<<"$section"

  printf '%s %s
' "$mark" "$name"
  printf '```text
%s
```

' "$section"
}

section "$usage" >> "$out"

in_subcommands=0
while read -r subcommand rest; do
  [[ $subcommand == "SUBCOMMANDS:" ]] && in_subcommands=1 && continue
  if ((in_subcommands)); then
<<<<<<< HEAD
      section "$(cargo -q run -p solana-cli -- help "$subcommand" | sed 's|'"$HOME"'|~|g')" "####" >> "$out"
=======
      section "$(cargo +"$rust_stable" -q run -p solana-cli -- help "$subcommand" | sed -e 's|'"$HOME"'|~|g' -e 's/[[:space:]]\+$//')" "####" >> "$out"
>>>>>>> d567799d4... Use $rust_stable
  fi
done <<<"$usage">>"$out"
