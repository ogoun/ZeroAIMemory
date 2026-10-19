# Moving a service to a new IP: DNS TTL planning

Notes from migrating the customer portal to a new hosting provider. The cutover itself is
one record change; the hard part is that resolvers all over the internet cache the old
answer for as long as the TTL allows, and some cache longer.

## Timeline that worked

1. **T-3 days**: check the current TTL. `dig +noall +answer portal.example.com A` shows it
   in the second column. Ours was 86400 (one day).
2. **T-2 days**: lower the TTL to 300. The lowering only takes effect once caches have
   expired the old record, so it has to happen at least one old TTL before the move.
3. **T-0**: bring up the new environment, verify it directly with
   `curl --resolve portal.example.com:443:203.0.113.20 https://portal.example.com/health`
   (no DNS involved), then change the A/AAAA records.
4. **T+0 to T+1 day**: keep the old servers running and serving the same data. Traffic
   trickles in from stale caches, misbehaving resolvers and long-lived clients (JVMs used
   to cache DNS forever with a security manager enabled; some mobile SDKs pin connections).
5. **T+2 days**: raise the TTL back to something like 3600 once the dust settles.

## Checking propagation

- Ask the authoritative servers directly: `dig @ns1.provider.net portal.example.com A`.
- Ask big public resolvers: `dig @1.1.1.1 ...`, `dig @8.8.8.8 ...`. The TTL they return
  counts down; it tells you how long until they ask again.
- Watch request logs on the old box rather than guessing.

## Gotchas

- **Negative caching**: if you query a name before it exists, the NXDOMAIN is cached for
  the SOA minimum TTL. Create new records before anyone looks them up.
- **CNAME at the apex** is not allowed; providers offer ALIAS/ANAME flattening instead.
- Changing **NS records** (moving DNS providers) is a separate, slower migration: the
  parent zone's delegation TTL is often 48 hours and you do not control it.
- Email: MX, SPF and DKIM records need to come along if the zone moves.
- TLS certificates for the new environment must be issued before cutover; HTTP-01
  validation will fail while DNS still points at the old host, so use DNS-01 or copy the cert.
