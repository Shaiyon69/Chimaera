# Security Policy

## Supported versions

Only the latest release of Chimaera gets security fixes. Update before reporting.

## Reporting a vulnerability

Do not post a vulnerability anywhere public. Report it privately to the maintainer, [Shaiyon69](https://github.com/Shaiyon69), by direct message.

Include:

- What the vulnerability is and what an attacker could do with it
- Steps to reproduce it
- The Chimaera version and operating system you found it on

You will get a reply when the report has been read. Fixed issues ship in the next release and are credited in its notes unless you ask otherwise.

## Scope

In scope: the Chimaera app in this repository, including its IPC handlers, deep links (`chimaera://`), the native addon and the Friends tab client.

Out of scope:

- The Hydra Launcher API (hydralauncher.gg) and upstream Hydra; report those to [Hydra](https://github.com/hydralauncher/hydra).
- Download sources and the files they serve. Chimaera does not vet third-party sources.
- The shared Supabase backend's server-side rules belong to [Messapp](https://github.com/Shaiyon69/MessApp), but reports about Chimaera's use of it are welcome here.
