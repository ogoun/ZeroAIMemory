---
tags: [ssh, linux, security]
---
# ~/.ssh/config, jump hosts and why we stopped using agent forwarding

## A config that saves typing

```
Host bastion
    HostName bastion.example.net
    User ops
    IdentityFile ~/.ssh/id_ed25519_work
    IdentitiesOnly yes

Host app-* db-*
    User deploy
    ProxyJump bastion
    IdentityFile ~/.ssh/id_ed25519_work
    IdentitiesOnly yes

Host *
    ServerAliveInterval 30
    ServerAliveCountMax 3
    AddKeysToAgent yes
```

Rules worth remembering:

- For each option the **first** matching value wins, so specific `Host` blocks go on top
  and `Host *` at the bottom.
- `IdentitiesOnly yes` stops the client from offering every key in the agent. Without it,
  servers with `MaxAuthTries 6` reject you with "Too many authentication failures" once
  you have enough keys loaded.
- `ssh -G app-3` prints the resolved configuration for a host — the fastest way to debug
  which block matched.
- Permissions: `~/.ssh` must be 700 and private keys 600, otherwise the client ignores
  the key or the server refuses `authorized_keys`.

## Agent forwarding (`ForwardAgent yes`, `ssh -A`)

Forwarding makes the remote host able to use your local agent through a socket in
`$SSH_AUTH_SOCK`. The key never leaves your machine, but anyone with root on that
remote host can use the socket while you are connected and authenticate as you anywhere
your key is accepted. On a shared bastion that is a real risk.

We replaced it with `ProxyJump` (`ssh -J bastion app-3`): the TCP stream is tunneled
through the bastion and authentication to `app-3` happens end-to-end from the laptop.
The bastion never sees the agent.

Where forwarding is unavoidable (e.g. `git pull` from a private repo on a remote host):

- enable it only for that host, never under `Host *`;
- add keys with confirmation: `ssh-add -c ~/.ssh/id_ed25519_work`, so every use pops a
  prompt locally;
- or better, use a deploy key on that host with read-only access to one repository.

Checking what the agent holds: `ssh-add -l`. Removing all keys: `ssh-add -D`.
