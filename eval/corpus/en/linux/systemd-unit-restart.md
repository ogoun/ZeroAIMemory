---
tags: [linux, systemd, services]
---
# systemd unit for a long-running service: restart policy, ordering, env files

Notes from moving our ingest worker off a `nohup ... &` script and onto a proper unit.
The final file lives in `/etc/systemd/system/ingest-worker.service`:

```ini
[Unit]
Description=Ingest worker
Wants=network-online.target
After=network-online.target postgresql.service
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=simple
User=ingest
EnvironmentFile=/etc/ingest/worker.env
EnvironmentFile=-/etc/ingest/worker.local.env
ExecStart=/opt/ingest/bin/worker --config /etc/ingest/worker.toml
Restart=on-failure
RestartSec=5s
TimeoutStopSec=30s

[Install]
WantedBy=multi-user.target
```

## Things that bit us

**After= is not a dependency.** `After=postgresql.service` only says "if both are being
started in the same transaction, start me later". It does not pull PostgreSQL in. If you
need the other unit to actually run, add `Wants=` (soft) or `Requires=` (hard) next to
`After=`. Same story for the network: `network-online.target` does nothing unless some
unit wants it, hence the `Wants=` line.

**Restart= values.** `on-failure` restarts on non-zero exit, on a signal other than the
clean ones, on watchdog timeout and on start timeout. `always` also restarts after a clean
exit 0 — handy for workers that exit on purpose after N jobs. `no` is the default, which is
why the first version of the unit silently stayed dead after a crash.

**Start limit.** With `RestartSec` too small the unit hits the rate limit
(`StartLimitBurst` starts inside `StartLimitIntervalSec`) and systemd gives up with
"start request repeated too quickly". Those two keys belong in `[Unit]`, not `[Service]`
— older blog posts put them in the wrong section and systemd just warns and ignores them.
After fixing the cause, `systemctl reset-failed ingest-worker` clears the counter.

**EnvironmentFile format.** Plain `KEY=value` lines, no `export`, no shell expansion,
quotes are stripped. A leading `-` in front of the path makes the file optional. Values
are visible to anyone who can run `systemctl show`, so real secrets go to
`LoadCredential=` instead.

## Workflow after editing

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now ingest-worker
systemctl status ingest-worker
systemd-analyze verify /etc/systemd/system/ingest-worker.service
```

For local overrides prefer `systemctl edit ingest-worker`, which creates a drop-in under
`ingest-worker.service.d/override.conf` and survives package upgrades.
