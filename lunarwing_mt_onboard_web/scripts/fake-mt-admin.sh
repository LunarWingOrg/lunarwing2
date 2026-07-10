#!/usr/bin/env bash
# Demo stand-in for lunarwing-mt-admin.sh. Emits realistic phase output with
# sleeps so the web UI can be exercised without sudo or system changes.
# A tenant name containing "fail" makes build-tenant fail (to demo error UX).
set -uo pipefail

SLEEP="${LUNARWING_DEMO_SLEEP:-0.3}"
nap() { sleep "$SLEEP"; }

cmd="${1:-}"
shift || true

# Resolve the tenant name: first positional arg, or the value after --tenant.
name="demo"
prev=""
for a in "$@"; do
  if [ "$prev" = "--tenant" ]; then name="$a"; break; fi
  case "$a" in
    --*) ;;
    *) name="$a"; break ;;
  esac
  prev="$a"
done

case "$cmd" in
  add-tenant)
    echo "=== Adding tenant: $name ==="
    echo "--- Allocating port block ---"; nap
    echo "allocated port block 10000-10009 for $name"; nap
    echo "--- Creating system user ---"; nap
    echo "created user '$name' (uid 4242, home /home/$name)"; nap
    echo "--- Cloning tenant repo ---"
    for p in 8 24 41 63 82 97 100; do echo "Receiving objects: ${p}% ($((p*37))/3700)"; nap; done
    echo "--- Generating environment files ---"; nap
    echo "wrote /home/$name/lunarwing/env/lunarwing.env"
    echo "wrote /home/$name/lunarwing/env/bridge.env"; nap
    echo "--- Provisioning SSH harness ---"; nap
    echo "generated ed25519 keypair for $name"; nap
    echo "--- Starting PostgreSQL container ---"; nap
    echo "created container lunarwing-pg-$name (pgvector/pgvector:pg16)"; nap
    echo "=== Tenant '$name' added ==="
    ;;

  build-tenant)
    echo "=== Building tenant: $name ==="
    echo "--- Building lunarwing binary (cargo) ---"
    for c in near-agent lunarwing-core tensorzero-client secrets-store gateway xmpp-bridge; do
      echo "   Compiling $c v1.1.9"
      nap
    done
    if [[ "$name" == *fail* ]]; then
      echo "error[E0499]: cannot borrow \`tenant\` as mutable more than once" >&2
      echo "=== Build FAILED for '$name' ===" >&2
      exit 1
    fi
    echo "    Finished \`release\` profile [optimized] target(s) in 42.7s"; nap
    echo "--- Building WASM extensions ---"; nap
    echo "installed 6 wasm extensions"; nap
    echo "--- Building worker images (podman) ---"
    for s in 1 2 3 4 5 6 7; do echo "STEP $s/7: RUN cargo build --release"; nap; done
    echo "=== Build complete for '$name' ==="
    ;;

  build-darkirc)
    echo "=== Building DarkIRC for tenant: $name ==="
    echo "--- Cloning darkfi ---"; nap
    for p in 15 45 78 100; do echo "Receiving objects: ${p}%"; nap; done
    echo "--- make darkirc ---"
    for c in drk darkirc net-utils; do echo "   Compiling $c"; nap; done
    echo "installed /usr/local/bin/darkirc"; nap
    echo "=== DarkIRC ready for '$name' ==="
    ;;

  start-tenant)
    echo "=== Starting tenant: $name ==="
    echo "--- Starting PostgreSQL ---"; nap
    echo "lunarwing-pg-$name is healthy"; nap
    echo "--- Starting vision sidecar ---"; nap
    echo "--- Starting daemon service ---"; nap
    echo "started lunarwing-$name"; nap
    echo "--- Starting workers ---"; nap
    echo "started nanocode, pebble, opencode workers"; nap
    echo "=== Tenant '$name' started ==="
    ;;

  *)
    echo "fake-mt-admin: unknown command: $cmd" >&2
    exit 2
    ;;
esac
