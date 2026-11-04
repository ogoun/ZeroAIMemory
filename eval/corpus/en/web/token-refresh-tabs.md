# Access token refresh in the storefront

The storefront keeps a short-lived access token (10 minutes) in memory and a refresh token in an
`HttpOnly` cookie. The refresh token is rotated: each use returns a new one and invalidates the
old one.

## When the token is refreshed

The API wrapper refreshes the access token one minute before it expires and also after a 401
response. Requests that arrive during a refresh wait for it instead of starting their own.

## Rotation and reuse detection

The auth service treats a second use of an already rotated refresh token as theft and revokes
the whole token family. The user is then logged out on every device.

## Logout

Logout calls the revoke endpoint and clears the in-memory token. Other open tabs learn about it
from a `storage` event and redirect to the login page.

## Testing

Token lifetime in the test environment is 30 seconds, so refresh paths are exercised by every
e2e run instead of only in long sessions.
