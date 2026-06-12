# Security Policy

## Supported Versions

OpenLogi is under active development and has not reached a stable 1.0 release.
Security fixes are provided for the latest public release and the current
development branch.

| Version | Supported |
| ------- | --------- |
| Latest release | :white_check_mark: |
| `master` | Best effort |
| Older releases | :x: |

If you are using an older release, please upgrade before reporting an issue
unless the vulnerability is still present in the latest release or on `master`.

## Reporting a Vulnerability

Please report suspected vulnerabilities privately by emailing:

`security@openlogi.org`

Do not open a public GitHub issue for a suspected vulnerability.

Useful reports include:

- A short description of the issue and its impact.
- Steps to reproduce, proof-of-concept code, or affected configuration.
- The OpenLogi version or commit, operating system version, device model, and
  connection type.
- Relevant logs or screenshots with private data removed.
- Whether the issue is already public or shared with anyone else.

Examples of issues that should be reported privately include:

- Arbitrary code execution, privilege escalation, or sandbox bypasses.
- Unsafe handling of configuration, profile, update, or asset data.
- Leaks of private configuration, logs, device identifiers, or user activity.
- Security-sensitive behavior in the event hook, IPC, updater, packaging, or
  device communication paths.

## Response Expectations

The maintainers aim to acknowledge new vulnerability reports within 7 days.
After triage, we will let you know whether the report is accepted, needs more
information, or is out of scope.

For accepted reports, we will coordinate a fix and disclosure timeline with the
reporter. We aim to provide status updates at least every 14 days while the
issue is being investigated or fixed.

If a report is declined, we will explain the reason when practical.

## Disclosure

Please give the maintainers a reasonable opportunity to investigate and release
a fix before publicly disclosing a vulnerability. Once a fix is available, we may
publish a security advisory, release notes, or upgrade guidance depending on the
severity and user impact.

OpenLogi does not currently operate a paid bug bounty program.
