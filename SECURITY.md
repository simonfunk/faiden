# Security policy

Faiden is an early-stage application, not a sandbox. Shells and Hermes run with the current user's authority. Notes, transcripts, and approval records are stored in plaintext SQLite. See the README for the full trust boundary.

## Reporting a vulnerability

Please use [GitHub private vulnerability reporting](https://github.com/simonfunk/faiden/security/advisories/new). Do not open public issues containing exploit details, credentials, transcripts, or personal data. If private reporting is unavailable, contact the maintainer through the contact options on [@simonfunk's profile](https://github.com/simonfunk) before sharing details.

Include the affected commit/version, operating system, a minimal sanitized reproduction, expected versus actual behavior, and impact. Never test against someone else's account or data.

Only the latest development version is currently maintained. There is no security response SLA or supported release series yet. This project has no independent security certification.
