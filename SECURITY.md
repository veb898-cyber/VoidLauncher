# Security policy

VoidLauncher handles Minecraft accounts, authentication tokens, modpack archives
and Windows processes. Security reports are taken seriously, and responsible
disclosure is appreciated.

## Supported versions

Security fixes are provided for the latest published VoidLauncher release.
Older releases may not receive fixes; please reproduce the issue on the newest
version before reporting it.

| Version | Supported |
| --- | --- |
| Latest published release | Yes |
| Older releases | No |

## Report a vulnerability privately

**Do not open a public issue for a security vulnerability.**

VoidLauncher uses GitHub's private vulnerability reporting:

1. Open the repository's **Security** tab.
2. Select **Report a vulnerability**.
3. Describe the issue and submit the advisory privately.

If the **Report a vulnerability** button is not available, contact the maintainer
[@veb898-cyber](https://github.com/veb898-cyber) through GitHub and ask for a
private channel before sharing any details. Do not include exploit details in a
public issue, discussion, pull request or social-media message.

This security policy explains the reporting process; the private reporting
setting itself is managed in the repository settings.

## What to include

A useful report contains:

- the VoidLauncher version and Windows version;
- the affected area (for example accounts, updater, modpack import, archive
  extraction, Rooms/VoidLink or the Tauri shell);
- a clear description of the impact and who could be affected;
- minimal reproduction steps or a proof of concept;
- relevant logs with tokens, passwords, session IDs, private keys and personal
  paths removed;
- any suggested fix or mitigation, if you have one.

Never send real access tokens, refresh tokens, passwords, session files or
private keys. Use a test account and synthetic data. If a secret was exposed,
say so without pasting the value, and rotate it.

## Scope

Reports about the following are in scope:

- the launcher's Rust and React code;
- account storage, Microsoft/Ely.by authentication and token handling;
- the auto-updater, installer and release metadata;
- modpack import, archive extraction and file verification;
- path traversal, symlink/reparse-point handling, command injection and
  privilege boundaries;
- leaks of secrets or personal data through logs, reports or the UI;
- vulnerabilities in dependencies that are reachable through VoidLauncher.

The following are generally out of scope:

- bugs in Minecraft, Mojang services, mods, resource packs or third-party
  servers;
- issues that require the attacker to already control the user's Windows
  account, filesystem or local network;
- denial of service caused only by a user editing their own local files;
- reports against versions that are no longer supported;
- social engineering and physical access attacks.

## Response

The maintainer aims to acknowledge a report within seven days, investigate it,
and keep the reporter informed about the fix and disclosure timeline. This is a
volunteer project, so these are targets rather than a contractual guarantee.

Please allow time for a fix before publishing details. Coordinated disclosure is
appreciated.

## Safe harbor

Good-faith security research is welcome. Please:

- test only against your own installation and accounts;
- do not access, modify or destroy other people's data;
- do not disrupt GitHub, Mojang, Modrinth, CurseForge or other services;
- stop and report immediately if you encounter personal data or a live secret;
- give the maintainer a reasonable opportunity to fix the issue before public
  disclosure.

## For contributors

When changing logging, diagnostics, bug reports, authentication or download
code, keep secrets out of logs and reports. `src-tauri/src/bug_report.rs` and
`src/lib/bugReport.ts` contain the redaction rules; add regression tests for any
new credential-like field. See [CONTRIBUTING.md](CONTRIBUTING.md) for the normal
development workflow.
