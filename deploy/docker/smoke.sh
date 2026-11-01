#!/bin/sh
set -eu
image=${1:?Pass the image or immutable digest to verify}
mkdir -p container-evidence
name="apc-acceptance-$$"
cleanup() {
    docker logs "$name" > container-evidence/container.log 2>&1 || true
    docker rm -f -v "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
docker run -d --name "$name" --read-only --cap-drop ALL \
    --security-opt no-new-privileges:true --tmpfs /tmp:mode=1777,size=268435456 \
    --network none "$image"
ready=false
for attempt in $(seq 1 60); do
    status=$(docker inspect --format '{{.State.Health.Status}}' "$name")
    if [ "$status" = healthy ]; then ready=true; break; fi
    if [ "$(docker inspect --format '{{.State.Running}}' "$name")" != true ]; then break; fi
    sleep 5
done
[ "$ready" = true ] || { docker logs "$name"; exit 1; }
docker exec "$name" python3 /app/container/smoke.py > container-evidence/hmi-smoke.json
docker exec "$name" python3 /app/container/transport-smoke.py > container-evidence/transport-smoke.json
# Restart preserves SQLite, research artifacts and Wine; sessions intentionally expire.
docker restart "$name" >/dev/null
for attempt in $(seq 1 60); do
    if [ "$(docker inspect --format '{{.State.Health.Status}}' "$name")" = healthy ]; then
        docker exec "$name" python3 /app/container/smoke.py --after-restart > container-evidence/restart-smoke.json
        exit 0
    fi
    sleep 5
done
exit 1
