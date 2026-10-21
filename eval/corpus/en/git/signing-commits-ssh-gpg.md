---
tags: [git, security, signing]
---
# Signing commits and tags: SSH keys vs GPG

Our main branch now requires signed commits. Most of us switched from GPG to SSH signing,
because everyone already has an SSH key and there is no keyring to babysit.

## SSH signing (Git 2.34+)

```bash
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/id_ed25519.pub
git config --global commit.gpgsign true
git config --global tag.gpgsign true
```

Despite the names (`gpg.format`, `commit.gpgsign`), nothing here uses GPG. With an agent you can
give the key literally: `user.signingkey "key::ssh-ed25519 AAAA..."`.

Verification locally needs an allowed-signers file, similar to `authorized_keys` but with an
e-mail in front:

```
# ~/.config/git/allowed_signers
alice@example.com ssh-ed25519 AAAAC3Nza...
bob@example.com   ssh-ed25519 AAAAC3Nzb...
```

```bash
git config --global gpg.ssh.allowedSignersFile ~/.config/git/allowed_signers
git log --show-signature -3
git verify-commit HEAD
git verify-tag v2.1.0
```

Without that file, `git log --show-signature` prints
`error: gpg.ssh.allowedSignersFile needs to be configured and exist for ssh signature
verification`, which looks scary but only means local verification is not set up. The hosting
platform verifies against the keys uploaded as *signing* keys (a separate key type from
authentication keys in the settings).

## GPG signing

```bash
gpg --full-generate-key                 # ed25519 or rsa4096
gpg --list-secret-keys --keyid-format=long
git config --global user.signingkey 3AA5C34371567BD2
git config --global commit.gpgsign true
```

Common failure: `error: gpg failed to sign the data`. Usual causes:

1. `GPG_TTY` not set in a terminal session: add `export GPG_TTY=$(tty)` to the shell profile.
2. The pinentry program cannot show a prompt (IDE, SSH session). Configure `pinentry-mode
   loopback` or a GUI pinentry.
3. The key expired. `gpg --edit-key <id>` then `expire`.
4. Git uses a different `gpg` binary than the one holding the key; set `gpg.program`.

Debug with `GIT_TRACE=1 git commit -S -m test` to see the exact command git runs.

## Comparison

| Aspect                | SSH                               | GPG                                 |
|-----------------------|-----------------------------------|-------------------------------------|
| Setup                 | minutes, existing key             | key generation, keyring, pinentry   |
| Expiry and revocation | none built in                     | yes                                 |
| Web of trust          | no                                | yes (rarely used in practice)       |
| Hardware keys         | `ed25519-sk` with FIDO2 tokens    | smartcards, YubiKey OpenPGP applet  |
| Local verification    | allowed signers file              | public keys in keyring              |

## Notes

- Signing proves who created the commit object, not who pushed it.
- Rebase and amend re-sign the new commits with *your* key, including other authors' commits
  you rebased. That is expected; the committer field changes too.
- A signed tag (`git tag -s v2.1.0 -m "..."`) is an annotated tag with a signature; lightweight
  tags cannot be signed.
- Squash-merges done by the hosting platform are signed by the platform's own key.
