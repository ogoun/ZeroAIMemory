---
tags: [http, caching, cdn]
---
# HTTP caching headers: Cache-Control, ETag and friends

A reference I keep coming back to whenever a deploy "doesn't show up" for some users, or the
CDN bill jumps because nothing is being cached. HTTP caching is controlled almost entirely by
response headers; getting them right per resource type solves both problems.

## The two questions a cache asks

1. **Is my stored copy still fresh?** If yes, serve it without contacting the origin.
   Freshness comes from `Cache-Control: max-age` (or `s-maxage` for shared caches), and
   historically from `Expires`.
2. **If it is stale, has it changed?** The cache sends a conditional request using a
   validator (`ETag` or `Last-Modified`). The origin answers `304 Not Modified` with no body
   if nothing changed, which saves bandwidth but still costs a round trip.

## Cache-Control directives that matter

| Directive | Meaning |
|-----------|---------|
| `max-age=N` | Fresh for N seconds in any cache |
| `s-maxage=N` | Overrides `max-age` for shared caches (CDN, proxies) only |
| `public` | Shared caches may store it, even for authenticated requests |
| `private` | Only the browser may store it; CDNs must not |
| `no-cache` | May be stored, but must be revalidated before every use |
| `no-store` | Must not be stored anywhere |
| `immutable` | Will never change while fresh; browsers skip revalidation on reload |
| `must-revalidate` | Once stale, must not be served without successful revalidation |
| `stale-while-revalidate=N` | May serve stale for N seconds while refreshing in background |
| `stale-if-error=N` | May serve stale for N seconds if the origin errors |

The most common confusion: `no-cache` does **not** mean "do not cache". It means "always check
with me first". If something must never touch disk (banking pages, tokens), that is `no-store`.

## Validators: ETag vs Last-Modified

`ETag` is an opaque version identifier for a representation, usually a hash of the content:

```
HTTP/1.1 200 OK
ETag: "5f3c-1a2b3c4d"
Cache-Control: no-cache
```

Next request from the browser:

```
GET /api/settings HTTP/1.1
If-None-Match: "5f3c-1a2b3c4d"
```

If unchanged, the server replies `304 Not Modified` with the same `ETag`.

- **Strong** ETags (`"abc"`) promise byte-for-byte identity; **weak** ones (`W/"abc"`) promise
  semantic equivalence. Compression changes bytes, so some servers weaken ETags when gzipping.
- `Last-Modified` / `If-Modified-Since` has one-second granularity and breaks when files are
  copied with new timestamps during deploy. Prefer ETag when you can compute one cheaply.
- Behind a load balancer, ETags must be identical across nodes. nginx builds them from mtime
  and size, so if the build step sets different mtimes on each server, every node produces a
  different ETag and revalidation never hits.

## Recipes by resource type

### Fingerprinted static assets

Files whose names contain a content hash (`app.3f9a1c.js`):

```
Cache-Control: public, max-age=31536000, immutable
```

A new build produces a new filename, so the old one can be cached for a year.

### HTML entry points

`index.html` references the fingerprinted assets, so it must update promptly:

```
Cache-Control: no-cache
ETag: "..."
```

Every navigation revalidates, but unchanged pages cost only a 304.

### API responses

- Per-user data: `Cache-Control: private, no-cache` plus ETag, or `no-store` for sensitive data.
- Public, slowly changing data: `public, max-age=60, stale-while-revalidate=300`. Readers get
  instant answers, and the origin sees at most one refresh per minute per CDN node.

### Things that must not be cached

```
Cache-Control: no-store
```

Login responses, anything with secrets, one-time links.

## Vary

`Vary` tells caches which request headers change the response. `Vary: Accept-Encoding` is
normal. `Vary: Cookie` or `Vary: User-Agent` effectively disables shared caching because
every distinct value is a separate cache entry. If the response differs by language, prefer
distinct URLs (`/en/`, `/de/`) over `Vary: Accept-Language`.

## Heuristic freshness: the default nobody chose

If a response has no `Cache-Control` and no `Expires` but does have `Last-Modified`, caches are
allowed to invent a freshness lifetime. The common heuristic is 10 % of the time since the
last modification: a file last changed 30 days ago may be considered fresh for three days.
That is how a forgotten header on a JSON config file led to clients running with a stale
feature flag for most of a weekend. Always send an explicit `Cache-Control`, even if it is
just `no-cache`.

Status codes matter too. `200`, `301`, `404` and `410` are cacheable by default when
freshness information is present or can be guessed; a `301` cached by a browser can live for
a very long time, which makes mistaken permanent redirects painful to undo. Use `302` or
`307` while you are still experimenting.

## CDN behaviour and purging

- Most CDNs respect `s-maxage` and `Cache-Control: private`, but defaults differ: some
  cache nothing without explicit headers, others cache static file extensions regardless.
  Read the provider's rules instead of assuming.
- Requests with an `Authorization` header are not stored by shared caches unless the
  response explicitly allows it (`public`, `s-maxage` or `must-revalidate`).
- Purging is the escape hatch, not the strategy. It only clears the CDN; browsers and
  corporate proxies keep their copies until their own `max-age` runs out.
- Surrogate keys (cache tags) let you purge every page that used a given product or article
  in one call, instead of listing URLs.
- When the origin is down, `stale-if-error` turns an outage into a period of slightly old
  pages, which for a catalog or documentation site is usually a good trade.

## Debugging checklist

- `curl -sI https://example.com/app.js` and read `Cache-Control`, `ETag`, `Age`. A non-zero
  `Age` means a shared cache served it.
- CDN-specific headers (`X-Cache: HIT`, `CF-Cache-Status`) show hits and misses.
- In browser devtools, "(memory cache)" or "(disk cache)" in the size column means no request
  was sent; a 304 means revalidation happened.
- Check that the framework does not add `Set-Cookie` to static responses: many CDNs refuse to
  cache anything that sets a cookie.
- After an incident where users kept getting an old bundle, the cause was `max-age=3600` on
  `index.html`. Purging the CDN did not help, because browsers held their own copies for an
  hour. Fix headers first, then purge.
