---
tags: [linux, systemd, scheduling, cron]
---
# Replacing cron jobs with systemd timers: when it is worth it and how

We had about twenty crontab lines on the reporting box, spread across root's crontab,
`/etc/cron.d/` and two user crontabs. After the third incident where a nightly export
"ran" but nobody could tell whether it succeeded, we moved most of them to systemd
timers. This note records the comparison, the conversion recipe and the traps.

## Why we moved

| Concern | cron | systemd timer |
|---|---|---|
| Logs | stdout mailed via MAILTO, or lost | captured in the journal per unit |
| Exit status | invisible unless the script logs it | `systemctl status`, `systemctl list-timers`, failed state |
| Overlap | two runs can overlap happily | a unit that is still running is not started again |
| Missed runs while powered off | skipped (anacron aside) | `Persistent=true` catches up after boot |
| Random spread | manual `sleep $((RANDOM % 600))` | `RandomizedDelaySec=` |
| Resource limits | none | any `[Service]` option: `MemoryMax=`, `Nice=`, `IOSchedulingClass=` |
| Environment | tiny, `PATH=/usr/bin:/bin` | same as other units, `EnvironmentFile=` works |
| Effort to add a job | one line | two files |

The last row is the honest downside. For a throwaway job on a dev box, one crontab line
is still the right tool.

## Conversion recipe

Old crontab line:

```
15 2 * * * /opt/reports/export.sh >> /var/log/export.log 2>&1
```

New service, `/etc/systemd/system/report-export.service`:

```ini
[Unit]
Description=Nightly report export
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
User=reports
ExecStart=/opt/reports/export.sh
Nice=10
MemoryMax=2G
```

New timer, `/etc/systemd/system/report-export.timer`:

```ini
[Unit]
Description=Run report export nightly

[Timer]
OnCalendar=*-*-* 02:15:00
Persistent=true
RandomizedDelaySec=5min
AccuracySec=1min

[Install]
WantedBy=timers.target
```

Activate the timer, not the service:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now report-export.timer
systemctl list-timers report-export.timer
```

The timer starts the service with the same name by default; use `Unit=` in `[Timer]` if
the names differ. `Type=oneshot` is important: the service is considered "active" while
the script runs, which is what prevents overlap and gives a meaningful exit status.

### OnCalendar syntax

`OnCalendar=` is not cron syntax. The general form is `DayOfWeek Year-Month-Day
Hour:Minute:Second`. Examples we used:

- `daily` — midnight;
- `Mon..Fri 08:00` — weekdays at eight;
- `*-*-01 03:00` — first day of every month;
- `*:0/15` — every 15 minutes.

Check an expression before deploying it:

```sh
systemd-analyze calendar "Mon..Fri 08:00" --iterations=3
```

It prints the normalized form and the next elapse times, which catches timezone
surprises. Timers use the system timezone unless the expression ends with a zone name
such as `UTC`.

### Monotonic timers

Besides wall-clock schedules there are relative ones: `OnBootSec=10min` and
`OnUnitActiveSec=1h` give "every hour, counting from the last activation". These have no
cron equivalent and work well for cache warmers where the exact minute does not matter.

## Traps

1. **Enabling the service instead of the timer.** `systemctl enable report-export.service`
   with an `[Install]` section would run it at boot; without one, it does nothing useful.
   Always enable the `.timer`.
2. **Forgetting daemon-reload.** Edits to unit files are not picked up until
   `systemctl daemon-reload`. `list-timers` keeps showing the old schedule.
3. **Persistent=true and long downtime.** After a week offline the job runs exactly once
   at boot, not seven times. That was what we wanted; if every missed run matters, the
   script itself has to backfill.
4. **Output.** Anything the script prints goes to the journal:
   `journalctl -u report-export.service --since today`. Remove the old `>> logfile`
   redirections, or you will look in the wrong place.
5. **Failure notification.** There is no MAILTO. We added `OnFailure=notify-failure@%n.service`
   in `[Unit]` of the service, where the template unit posts to our chat webhook.
6. **User timers.** Per-user timers live in `~/.config/systemd/user/` and are managed with
   `systemctl --user`. They stop when the user logs out unless lingering is enabled:
   `loginctl enable-linger reports`.

## Migration checklist per job

We converted jobs one at a time, each with the same steps:

1. Copy the crontab line into the ticket so the old schedule is documented.
2. Write the `.service`, run it once by hand with `systemctl start report-export.service`
   and read `journalctl -u report-export.service` — this catches missing environment
   variables before the schedule ever fires. Cron jobs often relied on variables defined
   at the top of the crontab; those move to `Environment=` or `EnvironmentFile=`.
3. Translate the schedule, verify it with `systemd-analyze calendar`.
4. Enable the timer, then comment out (not delete) the crontab line with a pointer to the
   unit name. Delete it after a week of clean runs.
5. Add the job to the dashboard that parses `systemctl list-timers --output=json`.

## Hardening options we added for free

Since the job is now an ordinary service, sandboxing directives apply. For the export
script these were enough and caught one bug where the script wrote to a stray path:

```ini
ProtectSystem=strict
ReadWritePaths=/var/lib/reports
PrivateTmp=true
NoNewPrivileges=true
ProtectHome=true
```

`systemd-analyze security report-export.service` scores the unit and lists what else
could be tightened. With cron, the equivalent would have required wrapping each script
in a separate sandboxing tool.

## What stayed in cron

Two jobs on a vendor appliance where we cannot add unit files, and a one-liner on a
personal VM. Everything else now shows up in `systemctl list-timers --all`, which became
the single place to answer "what runs on this box at night and did it work".
The `LAST` and `PASSED` columns there, combined with the failed state of the service,
replaced a homemade "heartbeat file" check that we used to maintain for every cron job.
