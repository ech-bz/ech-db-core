#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

STATE=/tmp/echtest
mkdir -p "$STATE"

FDB=ech-test-fdb
S3=ech-test-s3
SUI=ech-test-sui

sui=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --sui) sui=1; shift ;;
        --) shift; break ;;
        *) break ;;
    esac
done

cleanup() {
    docker rm -f "$FDB" "$S3" "$SUI" >/dev/null 2>&1 || :
}
trap cleanup EXIT
cleanup

docker run -d --name "$FDB" --network host \
    -e FDB_NETWORKING_MODE=host \
    -e FDB_PORT=4500 \
    --tmpfs /var/fdb/data:size=2g \
    foundationdb/foundationdb:7.3.63
timeout 60 bash -c "until docker exec $FDB cat /var/fdb/fdb.cluster > /dev/null 2>&1; do sleep 1; done"
docker exec "$FDB" cat /var/fdb/fdb.cluster > "$STATE/fdb.cluster"
timeout 120 bash -c "until fdbcli -C $STATE/fdb.cluster --timeout 5 --exec 'configure new single ssd' || fdbcli -C $STATE/fdb.cluster --timeout 5 --exec 'status minimal'; do sleep 1; done"
sleep 3

docker run -d --name "$S3" --network host \
    --tmpfs /data:size=1g \
    chrislusf/seaweedfs:3.80 \
    server -s3 -dir=/data -s3.port=8333 -master.port=9333 -volume.port=8080 -filer.port=8888
timeout 60 bash -c 'until [ "$(curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:8333)" != "000" ]; do sleep 1; done'
sleep 3

unset ECH_TEST_SUI_RPC ECH_TEST_SUI_PACKAGE_ID ECH_TEST_SUI_REGISTRY_ID ECH_TEST_SUI_PUBLISHER_CAP_ID ECH_TEST_SUI_PUBLISHER_KEY

if [ "$sui" = 1 ]; then
    docker run -d --name "$SUI" -p 9100:9100 \
        mysten/sui-tools:testnet \
        sui start --fullnode-rpc-port 9100
    timeout 180 bash -c "until docker exec $SUI test -f /root/.sui/sui_config/client.yaml; do sleep 1; done"
    timeout 180 bash -c "until docker exec $SUI sui client gas 2>/dev/null | grep -q 0x; do sleep 2; done"
    docker exec "$SUI" mkdir -p /workspace
    docker cp contracts/ech-anchor "$SUI:/workspace/ech-anchor"
    docker exec "$SUI" rm -rf /workspace/ech-anchor/build
    docker exec "$SUI" sui client test-publish --build-env localnet --gas-budget 500000000 --json /workspace/ech-anchor > "$STATE/sui-publish.json"
    address=$(docker exec "$SUI" sui client active-address)
    docker exec "$SUI" sui keytool export --key-identity "$address" | grep -o 'suiprivkey1[a-z0-9]*' > "$STATE/sui-privkey.txt"
    python3 - "$STATE" <<'PY'
import json
import pathlib
import sys

state = pathlib.Path(sys.argv[1])
charset = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
text = (state / "sui-privkey.txt").read_text().strip()
values = [charset.index(c) for c in text[text.rfind("1") + 1 :]][:-6]
accumulator = 0
bits = 0
raw = bytearray()
for value in values:
    accumulator = (accumulator << 5) | value
    bits += 5
    if bits >= 8:
        bits -= 8
        raw.append((accumulator >> bits) & 0xFF)
seed = bytes(raw[1:33])
(state / "sui.key").write_text(seed.hex() + "\n")

changes = json.load(open(state / "sui-publish.json"))["objectChanges"]
package = next(c["packageId"] for c in changes if c["type"] == "published")
registry = next(c["objectId"] for c in changes if c.get("objectType", "").endswith("::registry::Registry"))
cap = next(c["objectId"] for c in changes if c.get("objectType", "").endswith("::registry::PublisherCap"))
lines = [
    "ECH_TEST_SUI_RPC=http://127.0.0.1:9100",
    f"ECH_TEST_SUI_PACKAGE_ID={package}",
    f"ECH_TEST_SUI_REGISTRY_ID={registry}",
    f"ECH_TEST_SUI_PUBLISHER_CAP_ID={cap}",
    f"ECH_TEST_SUI_PUBLISHER_KEY={state}/sui.key",
]
(state / "sui.env").write_text("\n".join(lines) + "\n")
PY
    set -a
    . "$STATE/sui.env"
    set +a
fi

export ECH_TEST_FDB_CLUSTER_FILE="$STATE/fdb.cluster"
export ECH_TEST_S3_ENDPOINT=http://127.0.0.1:8333
export ECH_TEST_S3_REGION=us-east-1
export ECH_TEST_S3_BUCKET=ech-db
export ECH_TEST_S3_ACCESS_KEY=ech-access
export ECH_TEST_S3_SECRET_KEY=ech-secret

if [ "$#" -gt 0 ]; then
    "$@"
else
    cargo test -p ech-db-integration-tests --test e2e --test grpc -- --ignored --test-threads=1
    cargo test -p ech-db-fault-injection -- --ignored --test-threads=1
fi
