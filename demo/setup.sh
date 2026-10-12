# Builds a throwaway workspace for recording the README demo with VHS.
# Usage: source demo/setup.sh   (sourced by demo/demo.tape)
#
# Everything lives in a fresh temp dir: projects are filled with sparse files
# (big apparent size, no real disk use) and the global-cache tools are fake
# scripts first on PATH, so recording never touches the real caches.

KIKA_DEMO="$(mktemp -d "${KIKA_DEMO_TMP:-${TMPDIR:-/tmp}}/kika-demo.XXXXXX")"
export KIKA_DEMO

mkfile() { mkdir -p "$(dirname "$1")" && truncate -s "$2" "$1"; }
repo() { mkdir -p "$KIKA_DEMO/projects/$1/.git"; }

repo storefront
mkfile "$KIKA_DEMO/projects/storefront/node_modules/.pnpm/next/dist/server.js" 612M
mkfile "$KIKA_DEMO/projects/storefront/node_modules/typescript/lib/tsc.js" 98M
mkfile "$KIKA_DEMO/projects/storefront/.next/cache/webpack/client.pack" 241M
mkfile "$KIKA_DEMO/projects/storefront/apps/admin/node_modules/react/index.js" 133M

repo api-gateway
mkfile "$KIKA_DEMO/projects/api-gateway/Cargo.toml" 0
mkfile "$KIKA_DEMO/projects/api-gateway/target/debug/deps/libtokio.rlib" 1400M
mkfile "$KIKA_DEMO/projects/api-gateway/target/release/api-gateway" 380M

repo design-system
mkfile "$KIKA_DEMO/projects/design-system/node_modules/storybook/index.js" 455M
mkfile "$KIKA_DEMO/projects/design-system/dist/index.mjs" 12M
mkfile "$KIKA_DEMO/projects/design-system/src/Button.tsx.orig" 8K

repo landing-page
mkfile "$KIKA_DEMO/projects/landing-page/dist/assets/hero.webp" 37M
mkfile "$KIKA_DEMO/projects/landing-page/notes.tmp" 4K

mkfile "$KIKA_DEMO/projects/sketches/node_modules/three/build/three.module.js" 64M

# Build outputs only count next to a package.json.
for project in storefront storefront/apps/admin design-system landing-page sketches; do
    echo '{}' > "$KIKA_DEMO/projects/$project/package.json"
done

mkfile "$KIKA_DEMO/caches/uv/wheels/torch.whl" 7300M
mkfile "$KIKA_DEMO/caches/yarn/v6/npm-lodash.tgz" 2100M
mkfile "$KIKA_DEMO/caches/npm/_cacache/content-v2/sha512/blob" 940M
mkfile "$KIKA_DEMO/caches/pnpm/v10/files/00/blob" 1650M
mkfile "$KIKA_DEMO/caches/go-build/a1/obj" 1200M
mkfile "$KIKA_DEMO/caches/go-mod/cache/download/golang.org/x/net.zip" 410M

mkdir -p "$KIKA_DEMO/bin"
fake() {
    printf '#!/bin/sh\n%s\n' "$2" > "$KIKA_DEMO/bin/$1"
    chmod +x "$KIKA_DEMO/bin/$1"
}
fake uv     "[ \"\$1\" = cache ] && [ \"\$2\" = dir ] && echo $KIKA_DEMO/caches/uv; exit 0"
fake yarn   "[ \"\$1\" = cache ] && [ \"\$2\" = dir ] && echo $KIKA_DEMO/caches/yarn; exit 0"
fake npm    "[ \"\$1\" = config ] && echo $KIKA_DEMO/caches/npm; exit 0"
fake pnpm   "[ \"\$1\" = store ] && [ \"\$2\" = path ] && echo $KIKA_DEMO/caches/pnpm; exit 0"
fake go     "[ \"\$1\" = env ] && [ \"\$2\" = GOCACHE ] && echo $KIKA_DEMO/caches/go-build; [ \"\$1\" = env ] && [ \"\$2\" = GOMODCACHE ] && echo $KIKA_DEMO/caches/go-mod; exit 0"
fake docker "[ \"\$1\" = system ] && printf 'Images\t0B (0%%)\nBuild Cache\t3.1GB (100%%)\n'; [ \"\$1\" = builder ] && printf 'Total:\t3.1GB\n'; exit 0"

export PATH="$KIKA_DEMO/bin:$PATH"
export PS1='\[\e[1;35m\]~/projects\[\e[0m\] \[\e[2m\]❯\[\e[0m\] '
cd "$KIKA_DEMO/projects" || return
