# Named volumes vs bind mounts, and the uid mismatch behind "Permission denied"

## Short version

| | Named volume | Bind mount |
|---|---|---|
| Syntax | `-v pgdata:/var/lib/postgresql/data` | `-v ./src:/app/src` or `--mount type=bind,...` |
| Where data lives | `/var/lib/docker/volumes/<name>/_data`, managed by Docker | any host path you choose |
| Initial content | copied from the image path on first use if the volume is empty | host directory hides whatever the image had there |
| Ownership | inherits owner of the image directory on first copy | whatever the host files have |
| Typical use | databases, state the host does not need to edit | source code during development, config files |
| Docker Desktop speed | fast (inside the VM) | slow on macOS/Windows for many small files |

Rule of thumb: state that belongs to the container goes to a named volume; files a human
edits on the host go to a bind mount.

## The uid problem

Containers do not have their own users at the kernel level. A process running as uid
1000 inside the container is uid 1000 on the host. The names in `/etc/passwd` differ, the
numbers are what count.

Incident: the dev compose file bind-mounted `./storage` into a PHP container that runs as
`www-data` (uid 33). On the developer's machine `./storage` belonged to uid 1000, so the
app failed with `Permission denied` writing cache files. Someone "fixed" it with
`chmod -R 777`, which then got committed.

Proper options:

1. **Run the container as the host user** for development: `user: "${UID}:${GID}"` in
   compose (export `UID`/`GID` in the shell or `.env`, bash does not export `UID` by
   default).
2. **Build the image with a matching uid**: `ARG APP_UID=1000` and
   `usermod -u $APP_UID www-data` in the Dockerfile.
3. **Use a named volume** for directories only the container writes (cache, sessions).
   Docker copies the image's directory with its ownership the first time, so the
   permissions are correct without touching the host.
4. **chown in an entrypoint** — works, but slow on big trees and runs on every start.

Note on the first-use copy: it happens only for named volumes and only when the volume is
empty. Bind mounts never copy anything; if you bind-mount an empty host directory over
`/app/node_modules`, the modules from the image disappear from the container's view.

On SELinux hosts (Fedora, RHEL) a bind mount may also fail with permission errors even
with correct uids; add `:z` (shared) or `:Z` (private) to relabel the host directory.
