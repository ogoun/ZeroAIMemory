# Browser HTTP cache for storefront assets

The storefront relies on the browser HTTP cache for its static assets and product images. The
headers are set by the edge, but the frontend build decides which files can be cached forever.

## Hashed assets

JavaScript, CSS and fonts have a content hash in the file name and are served with
`Cache-Control: public, max-age=31536000, immutable`. A new release produces new names, so old
files never need to expire.

## Product images

Images are served with `max-age=86400` and `stale-while-revalidate=604800`. After a day the
browser shows the cached image at once and revalidates it in the background with `If-None-Match`.

## The HTML shell

The HTML shell is served with `Cache-Control: no-cache`: the browser may store it but must
revalidate it on every load, so a release becomes visible immediately.

## Checking

The network panel shows `(disk cache)` or `(memory cache)` for cached responses and `304` for
revalidated ones. A cached response without a validator (`ETag` or `Last-Modified`) is a sign of
missing headers.
