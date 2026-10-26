---
index: false
---
# Trusted proxies: getting the real client IP without letting clients spoof it

Short version of a security review finding. Our app read the client address from the first
entry of `X-Forwarded-For`. Any client can send that header, so audit logs and per-IP rate
limits could be faked with a single `curl -H`.

How to do it properly:

1. Configure an explicit list of trusted proxy addresses or CIDRs (the load balancer subnet,
   the local reverse proxy). Never "trust all".
2. Walk `X-Forwarded-For` from **right to left**, skipping addresses that belong to trusted
   proxies. The first untrusted address is the client.
3. Only honour `X-Forwarded-Proto` and `X-Forwarded-Host` when the direct peer is trusted;
   otherwise a client can make the app believe the request came over HTTPS.
4. The outermost proxy should overwrite, not append to, the incoming header.

Framework knobs: Express `app.set('trust proxy', ['10.0.0.0/8'])`, ASP.NET Core
`ForwardedHeadersOptions.KnownNetworks`, Django has no built-in client IP logic (use a vetted
middleware), Rails `config.action_dispatch.trusted_proxies`.

Test: send a forged header from outside and confirm logs show your real address.
