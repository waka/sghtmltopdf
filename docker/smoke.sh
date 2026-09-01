#!/usr/bin/env bash
# Smoke test for the official Docker image. Used from CI and locally.
#
#   docker/smoke.sh ghcr.io/waka/sghtmltopdf:latest
#   PLATFORM=linux/arm64 docker/smoke.sh sghtmltopdf:arm64   # to run under QEMU
#
# Three things are checked:
#   1. The binary runs on that architecture (--version).
#   2. The CLI can produce a Japanese PDF (i.e. the bundled fonts work;
#      a tofu warning means the fonts were not found).
#   3. With no arguments it starts as a server, and /healthz and /pdf are
#      reachable from outside the container (`--listen 0.0.0.0` from CMD is in effect).
set -euo pipefail

image="${1:?usage: docker/smoke.sh <image> }"
platform_args=()
if [ -n "${PLATFORM:-}" ]; then
    platform_args=(--platform "$PLATFORM")
fi

workdir="$(mktemp -d)"
container="sghtmltopdf-smoke-$$"
cleanup() {
    docker rm -f "$container" >/dev/null 2>&1 || true
    rm -rf "$workdir"
}
trap cleanup EXIT

cat > "$workdir/smoke.html" <<'HTML'
<!doctype html>
<html lang="ja"><body>
<h1>請求書</h1>
<p style="font-family: sans-serif">ゴシック体の日本語 Gothic 123</p>
<p style="font-family: serif">明朝体の日本語 Mincho 123</p>
<p style="font-weight: bold">太字の日本語</p>
</body></html>
HTML

echo "== 1. --version"
docker run --rm "${platform_args[@]}" "$image" --version

echo "== 2. converting with the CLI"
# Pass --user so the output file is written with the host user as owner (see Dockerfile).
docker run --rm "${platform_args[@]}" --user "$(id -u):$(id -g)" \
    -v "$workdir:/work" -w /work "$image" smoke.html -o out.pdf 2> "$workdir/stderr.txt" || {
    cat "$workdir/stderr.txt" >&2
    echo "::error::the CLI conversion failed" >&2
    exit 1
}
cat "$workdir/stderr.txt"
if grep -q "tofu" "$workdir/stderr.txt"; then
    echo "::error::the bundled fonts cannot draw some characters (the fonts may not have been found)" >&2
    exit 1
fi
head -c 5 "$workdir/out.pdf" | grep -q "%PDF" || {
    echo "::error::the output is not a PDF" >&2
    exit 1
}
size=$(wc -c < "$workdir/out.pdf")
# With Japanese glyphs embedded the file is several KB or more (this tells it apart from an empty PDF).
if [ "$size" -lt 5000 ]; then
    echo "::error::the PDF is too small (${size} bytes). The fonts may not have been embedded" >&2
    exit 1
fi
echo "   -> produced a ${size}-byte PDF"

echo "== 3. starting it as a server"
docker run -d --name "$container" "${platform_args[@]}" \
    -p 127.0.0.1:18080:8080 "$image" >/dev/null
for _ in $(seq 1 60); do
    if curl -fsS http://127.0.0.1:18080/healthz >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
curl -fsS http://127.0.0.1:18080/healthz || {
    docker logs "$container" >&2
    echo "::error::/healthz is unreachable" >&2
    exit 1
}
echo
curl -fsS http://127.0.0.1:18080/version
echo
status=$(curl -sS -o "$workdir/server.pdf" -w '%{http_code}' \
    -X POST -H 'Content-Type: text/html' \
    --data-binary "@$workdir/smoke.html" http://127.0.0.1:18080/pdf)
if [ "$status" != "200" ]; then
    docker logs "$container" >&2
    echo "::error::/pdf returned $status" >&2
    exit 1
fi
head -c 5 "$workdir/server.pdf" | grep -q "%PDF" || {
    echo "::error::the server's output is not a PDF" >&2
    exit 1
}
echo "   -> the server also returned a $(wc -c < "$workdir/server.pdf")-byte PDF"

echo "== smoke test passed: $image"
