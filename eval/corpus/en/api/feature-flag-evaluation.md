# Feature flag evaluation in backend services

Backend services evaluate flags with an in-process SDK. The flag service only distributes rule
sets; it is not called on every request.

## Rule sets

A rule set lists flags with targeting rules: percentage rollout by user id, allow lists of
accounts and regions. The SDK downloads the full rule set at startup and then subscribes to
updates over a streaming connection.

## Evaluation

Evaluation is a pure function of the rule set and the request context (user id, region,
account type). The same user always gets the same variant because the percentage bucket is a
hash of the flag name and the user id.

## Defaults

Every flag check in code passes a default. The default is returned when the flag is unknown,
when the rule set has not loaded yet, or when the context lacks a required attribute.

## Audit

Changes to rule sets are made through the admin UI with a mandatory comment. The flag service
keeps the history, and each change is posted to the release channel.
