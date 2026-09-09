#!/usr/bin/env bash
# Build the browser island (crates/ghinvite-island) and stage it as the Static
# Assets directory the Worker deploys and the native binary serves:
#
#   dist/public/assets/ghinvite-island-dxh<hash>.js       hashed dx bundle (JS glue)
#   dist/public/assets/ghinvite-island_bg-dxh<hash>.wasm  hashed dx bundle (wasm)
#   dist/public/assets/ghinvite-island.js                 stable loader the SSR page references
#   dist/public/_headers                                  cache headers for Cloudflare Static Assets
#
# The SSR page only ever references the stable name
# (`ghinvite_ui::link_form::LINK_FORM_ISLAND_MODULE_SRC`), so the Rust build
# never learns the hash. Nothing else may go into dist/public — in particular
# no index.html, which Static Assets would serve for `/` (see wrangler/web.toml).
#
# Requirements: the Dioxus CLI (`dx` 0.7.x) and the wasm32-unknown-unknown
# target. `dx` runs the `cargo` on PATH, so keep the rustup-managed one first
# for rust-toolchain.toml to apply. Run from anywhere; paths are repo-relative.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

package="ghinvite-island"
out_dir="dist/public"
size_budget_kb=600   # gzipped wasm + js, the #36 acceptance criterion

command -v dx >/dev/null || { echo "error: dx (Dioxus CLI) not found on PATH; cargo binstall dioxus-cli@0.7.9" >&2; exit 1; }
command -v node >/dev/null || { echo "error: node not found on PATH (used to read dx's manifest)" >&2; exit 1; }

echo "==> dx bundle -p $package --platform web --profile island"
# `[profile.island]` (root Cargo.toml): opt-level=z, lto, codegen-units=1,
# panic=abort, strip. dx's own `wasm-release` profile only adds opt-level=s on
# top of `release`, which here has no size tuning (the Workers build shares
# it), and produces a ~30% larger wasm. dx still runs wasm-bindgen and
# wasm-opt -Oz. --debug-symbols false drops DWARF from the shipped wasm.
started="$(mktemp)"
dx bundle -p "$package" --platform web --profile island --debug-symbols false

# dx writes custom-profile builds under the same `release/web` output dir.
manifest="target/dx/$package/release/web/.manifest.json"
public="target/dx/$package/release/web/public"
[[ -f "$manifest" ]] || { echo "error: $manifest not found after dx bundle" >&2; exit 1; }
[[ "$manifest" -nt "$started" ]] || { echo "error: $manifest is stale (dx wrote its output elsewhere?)" >&2; exit 1; }
rm -f "$started"

# The bundled (hashed) file names from dx's manifest. The public/assets
# directory accumulates stale hashes across builds, so only the files the
# manifest lists are copied. (No `mapfile`: macOS ships bash 3.2.)
js=""; wasm=""
while IFS= read -r name; do
  case "$name" in
    *.js)   js="$name" ;;
    *.wasm) wasm="$name" ;;
    *)      echo "warning: ignoring unexpected bundled asset $name" >&2 ;;
  esac
done < <(node -e '
  const manifest = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
  for (const entries of Object.values(manifest.assets)) for (const e of entries) console.log(e.bundled_path);
' "$manifest")
[[ -n "$js" && -n "$wasm" ]] || { echo "error: manifest lists no .js/.wasm pair" >&2; exit 1; }
[[ -f "$public/assets/$js" && -f "$public/assets/$wasm" ]] || { echo "error: bundled files missing under $public/assets" >&2; exit 1; }

echo "==> staging $out_dir"
rm -rf "$out_dir"
mkdir -p "$out_dir/assets"
cp "$public/assets/$js" "$public/assets/$wasm" "$out_dir/assets/"

# Stable entry: the module the SSR page loads. It only imports the hashed
# bundle, which self-initialises (fetches its wasm by the absolute /assets/
# path dx baked in) and calls main().
printf 'import "/assets/%s";\n' "$js" > "$out_dir/assets/ghinvite-island.js"

# Cloudflare Static Assets default to `max-age=0, must-revalidate` + ETag.
# Hashed files can be cached forever; the loader must revalidate so a new
# deploy is picked up.
cat > "$out_dir/_headers" <<'EOF'
/assets/*-dxh*
  Cache-Control: public, max-age=31536000, immutable

/assets/ghinvite-island.js
  Cache-Control: no-cache
EOF

echo "==> sizes"
size_of() { wc -c < "$1" | tr -d ' '; }
gz_of()   { gzip -9 -c "$1" | wc -c | tr -d ' '; }
kb()      { echo "$(( ($1 + 1023) / 1024 ))"; }

wasm_raw=$(size_of "$out_dir/assets/$wasm"); wasm_gz=$(gz_of "$out_dir/assets/$wasm")
js_raw=$(size_of "$out_dir/assets/$js");     js_gz=$(gz_of "$out_dir/assets/$js")
total_gz=$(( wasm_gz + js_gz ))

printf '  %-52s %7s KB raw  %6s KB gzip\n' "$wasm" "$(kb "$wasm_raw")" "$(kb "$wasm_gz")"
printf '  %-52s %7s KB raw  %6s KB gzip\n' "$js"   "$(kb "$js_raw")"   "$(kb "$js_gz")"
printf '  %-52s %7s KB raw  %6s KB gzip  (budget %s KB)\n' "total" "$(kb $(( wasm_raw + js_raw )))" "$(kb "$total_gz")" "$size_budget_kb"

if (( total_gz > size_budget_kb * 1024 )); then
  echo "error: gzipped island bundle ($(kb "$total_gz") KB) exceeds the $size_budget_kb KB budget" >&2
  exit 1
fi

echo "==> done: $out_dir"
find "$out_dir" -type f | sort
