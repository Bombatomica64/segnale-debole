#!/usr/bin/env bash
# Build the web + API images and roll them out on the local single-node k3s.
#
#   deploy/deploy-k3s.sh            # TAG=v0.5.0
#   TAG=v0.5.1 deploy/deploy-k3s.sh
#
# Images are imported straight into k3s's containerd (imagePullPolicy: Never),
# so no registry is involved. Run from anywhere; paths resolve to the repo root.
# Note: deploy/k8s.yaml pins the image tags; bump them there when changing TAG.
set -euo pipefail

TAG="${TAG:-v0.5.0}"
NAMESPACE="segnale-debole"
SECRET="segnale-debole-secrets"
WEB_IMAGE="docker.io/library/segnale-debole-web:${TAG}"
API_IMAGE="docker.io/library/segnale-debole-api:${TAG}"
KUBECTL="${KUBECTL:-kubectl}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for tool in trunk docker "$KUBECTL" sudo; do
    command -v "$tool" >/dev/null 2>&1 || { echo "error: '$tool' not found in PATH" >&2; exit 1; }
done

if ! grep -q "segnale-debole-web:${TAG}" deploy/k8s.yaml || ! grep -q "segnale-debole-api:${TAG}" deploy/k8s.yaml; then
    echo "error: deploy/k8s.yaml does not pin tag ${TAG}; update deploy/k8s.yaml first so the deployed tag matches" >&2
    exit 1
fi

# The API needs DATABASE_URL + OIDC_CLIENT_SECRET from this Secret.
"$KUBECTL" get namespace "$NAMESPACE" >/dev/null 2>&1 || "$KUBECTL" create namespace "$NAMESPACE"
if ! "$KUBECTL" -n "$NAMESPACE" get secret "$SECRET" >/dev/null 2>&1; then
    cat >&2 <<MSG
error: Secret ${NAMESPACE}/${SECRET} is missing. Create it first (fill in the placeholders):

  ${KUBECTL} -n ${NAMESPACE} create secret generic ${SECRET} \\
    --from-literal=DATABASE_URL='postgres://<user>:<password>@<host>:5432/<db>' \\
    --from-literal=OIDC_CLIENT_SECRET='<client secret from Authentik>'
MSG
    exit 1
fi

echo "==> trunk build (release, audio, wasm-release profile)"
trunk build --release --features audio --cargo-profile wasm-release

echo "==> docker build ${WEB_IMAGE}"
docker build -f deploy/Dockerfile -t "$WEB_IMAGE" .
echo "==> docker build ${API_IMAGE}"
docker build -f deploy/Dockerfile.server -t "$API_IMAGE" .

echo "==> importing images into k3s containerd"
docker save "$WEB_IMAGE" | sudo k3s ctr images import -
docker save "$API_IMAGE" | sudo k3s ctr images import -

echo "==> applying deploy/k8s.yaml"
"$KUBECTL" apply -f deploy/k8s.yaml
# Same tag re-imported => force new pods so they pick up the fresh image.
"$KUBECTL" -n "$NAMESPACE" rollout restart deploy/segnale-debole deploy/segnale-debole-api
"$KUBECTL" -n "$NAMESPACE" rollout status deploy/segnale-debole --timeout=180s
"$KUBECTL" -n "$NAMESPACE" rollout status deploy/segnale-debole-api --timeout=180s

echo "==> deployed ${TAG}: https://signal.bombatomica64.dev (API health: /api/v1/healthz)"
