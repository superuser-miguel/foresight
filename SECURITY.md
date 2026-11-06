# Security

## Reporting a vulnerability

Please report it privately, through GitHub:
**[Report a vulnerability](https://github.com/superuser-miguel/foresight/security/advisories/new)**
(the *Security* tab → *Report a vulnerability*). Don't open a public issue for it.

Foresight has one maintainer. I'll reply as soon as I can and keep you posted
in the advisory until it is fixed or closed. Say whether you'd like credit.

## Supported versions

Only the **latest release** gets security fixes. A fix ships as a new release
through the Flatpak repo and GitHub Releases.

## What Foresight does with your keys and files

These are the promises a report would most likely be about:

- **Your private SSH keys never enter the sandbox.** Remote sync uses your SSH
  agent through `--socket=ssh-auth`. The agent stays outside the sandbox and
  only signs on request, so the key itself is never visible to Foresight.
- **`~/.ssh` is never read or written.** Foresight keeps its own `known_hosts`
  in its config folder. A host is trusted only after you confirm its
  fingerprint, and transfers run with `StrictHostKeyChecking=yes`, so an
  unknown or changed host key stops the transfer.
- **No access to your home folder.** Folders come through the file chooser
  portal, one at a time, as you pick them. The sandbox has no
  `--filesystem=home` or `--filesystem=host`.
- **No shell.** rsync is started with an argument list, never through a shell,
  so a file name or pattern can't run a command. The one string rsync parses
  itself, the `-e` ssh command, is quoted or refused (see `ssh.rs`).

A way around any of these is a vulnerability. Please report it.

## rsync itself

Foresight bundles its own rsync. A flaw in rsync belongs upstream at
[rsync's security policy](https://github.com/RsyncProject/rsync/security).
It's still worth telling me, so the bundled copy gets bumped.
