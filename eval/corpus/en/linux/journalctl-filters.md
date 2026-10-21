# journalctl filters I actually use

Cheat sheet for digging through the systemd journal without piping everything into grep.

```sh
journalctl -u ingest-worker -b            # one unit, current boot only
journalctl -u nginx -u php-fpm --since "1 hour ago"
journalctl -p err -b -1                   # errors and worse from the previous boot
journalctl -k                             # kernel ring buffer, like dmesg but persistent
journalctl _PID=4211                      # by field; see `journalctl -o verbose` for fields
journalctl -f -n 50 -u ingest-worker      # follow, starting from the last 50 lines
journalctl --since 09:00 --until 09:15 -o short-iso
```

Notes:

- `-b -1` only works if the journal is persistent. On many distros `/var/log/journal`
  does not exist by default and logs live in `/run` until reboot. Create the directory
  (or set `Storage=persistent` in `journald.conf`) and restart `systemd-journald`.
- Priority filter `-p warning..err` takes a range.
- `--grep` matches the MESSAGE field with a regex, case-insensitive if the pattern is
  all lowercase.
- Disk use: `journalctl --disk-usage`, trim with `--vacuum-size=500M` or `--vacuum-time=2weeks`.
