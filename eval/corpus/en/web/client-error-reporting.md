# Client-side error reporting in the storefront

Errors that happen in customers' browsers never reach server logs on their own. The storefront
reports them to a small collector endpoint so that crashes are visible on the same dashboards as
backend errors.

## What is captured

The reporter listens to `window.onerror` and `unhandledrejection`, and the root error boundary
of the UI framework calls it for render errors. Each report carries the message, the stack,
the release version, the route and a random session id; no user data and no URL query strings.

## Sending

Reports are batched and sent with `navigator.sendBeacon` when the page is hidden, so they
survive navigation. Identical reports within one page view are counted instead of sent again,
and one page view sends at most 20 reports.

## Source maps

Production bundles are minified. Source maps are uploaded to the collector during the release
build and are not served publicly; the collector resolves stack frames to original files.

## Triage

The dashboard groups reports by resolved top frame. A new group after a release is reviewed by
the frontend on-call the same day.
