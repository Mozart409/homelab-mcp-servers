# Container images do not trust the homelab step-ca root

A server running in its container cannot reach a backend whose certificate was
issued by the homelab step-ca. The same binary on the host works fine.

Verified 2026-09-08 with `alertmanagermcp-server:dev` pointed at
`https://alertmanager.homelab.internal`:

```
request to https://alertmanager.homelab.internal/api/v2/status failed:
  error sending request ... client error (Connect):
  invalid peer certificate: UnknownIssuer
```

The same call from the host binary returns the live config, so this is a trust
store problem, not a code or rustls-provider problem.

## Why

`Containerfile` copies exactly one thing into the runtime image:

```
COPY --from=build /app/server /usr/local/bin/server
```

The base is `gcr.io/distroless/static-debian12:nonroot`, which ships public CA
roots only. Nothing adds the homelab root, so any internally-issued certificate
is `UnknownIssuer`.

`rustls-platform-verifier` reads the system trust store, which is why this works
on the host and on the NixOS deployment — those machines have the root
installed. Only the container is missing it.

## Not specific to alertmanager-mcp

`.env.example` points two other servers at TLS backends that are plausibly
step-ca issued:

- `PBS_HOST=https://pbs.lan:8007`
- `WP_HOST=https://ci.homelab.local`

Any server whose target uses the internal CA is affected the moment it runs from
a container rather than the host. This is a pre-existing gap that adding
`alertmanager-mcp` surfaced; it did not introduce it.

Prior art, and why it does not cover this: `.woodpecker/homelab-ca.crt` and the
commits `trust the step-ca root and probe from busybox, curl, and nix images`
and `scope the step-ca note to homelab.local, not tailnet certs` solved the
trust problem for the **CI pipeline**, not for the shipped runtime images.

## Options

1. **Mount the root at runtime (recommended).** Bind-mount the CA into the
   container and point rustls at it with `SSL_CERT_FILE`, in `compose.yaml` and
   the systemd unit. No rebuild, and — the reason to prefer it — no
   homelab-specific certificate baked into an image that gets published to a
   public GHCR repository.
2. **`COPY` the root into the image** and append it to
   `/etc/ssl/certs/ca-certificates.crt`. Simplest to consume, but bakes a
   site-specific trust anchor into every published image, including the ones
   that are meant to be pullable from outside the tailnet. Reasonable for the
   internal Harbor images only, which would mean two build paths.
3. **`*_INSECURE=true`.** Rejected. It disables verification wholesale rather
   than trusting one more issuer, and it would be set in exactly the deployment
   where the traffic is most worth protecting.
4. **Do nothing** and run these servers on the host, as the NixOS module does
   today. Legitimate — but then `compose.yaml` is misleading for any https
   backend, and `just smoke-live` will keep failing for reasons unrelated to
   the code.

Option 1 also keeps `just smoke-live` honest: it is precisely the check that
caught this.

## Watch out for

Whichever option is taken, verify with `just smoke-live <bin>` rather than a
plain `curl` from the host — the host trusts the root already, so a host-side
curl passes while the container still fails.
