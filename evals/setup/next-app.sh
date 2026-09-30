#!/bin/sh
# Generate a realistic install footprint in the fresh working tree: a large
# node_modules/ and a .next/ build output, both full of decoys that mention the
# app's own identifiers (formatPrice, Cart, checkout). A filtered search must
# never surface them; an unfiltered one drowns in them.
set -eu
i=0
for pkg in react react-dom next lodash date-fns zod @types/node @types/react typescript eslint; do
    dir="node_modules/$pkg"
    mkdir -p "$dir/dist" "$dir/lib"
    printf '{"name":"%s","version":"1.0.0","main":"dist/index.js"}\n' "$pkg" > "$dir/package.json"
    n=0
    while [ "$n" -lt 300 ]; do
        printf 'export function formatPrice%s(c){return c/100}\nexport class Cart%s{total(){return 0}}\n// checkout helper %s\n' \
            "$n" "$n" "$n" > "$dir/lib/mod$n.js"
        n=$((n + 1))
        i=$((i + 1))
    done
    printf '/* minified */' > "$dir/dist/index.js"
    head -c 20000 /dev/zero | tr '\0' 'x' >> "$dir/dist/index.js"
done
mkdir -p .next/server/app/checkout .next/static/chunks
n=0
while [ "$n" -lt 200 ]; do
    printf 'self.__next_f.push([1,"formatPrice Cart checkout %s"])\n' "$n" > ".next/static/chunks/$n.js"
    n=$((n + 1))
done
printf '<html>checkout</html>\n' > .next/server/app/checkout/page.html
