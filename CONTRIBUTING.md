# Contributing to Faiden

Contributions are welcome: reproducible bug reports, documentation, accessibility and keyboard improvements, tests, and focused fixes. For a new adapter or architectural change, open an issue first to agree on scope.

## Development

Follow the prerequisites and commands in [README.md](README.md#development). macOS is the currently verified platform; do not claim Windows/Linux support without native evidence.

1. Fork the repository and create a focused branch.
2. For behavior changes, add a failing regression test first, then implement the smallest fix.
3. Run `pnpm typecheck`, `pnpm test`, and `pnpm build` from the root; run `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets -- -D warnings` from `src-tauri`.
4. For native lifecycle/UI changes, also test a separate development app and disposable data directory. Report exactly what you exercised, including restart and cleanup behavior.
5. Submit a PR describing the problem, approach, test results, and known limitations.

## Safety and correctness

- Tests use fixtures and temporary databases/repositories, never personal data. Automated tests must not call paid providers.
- Real provider canaries are separate, explicit, bounded checks, not ordinary unit tests.
- Never commit credentials, transcripts, personal screenshots, database files, build bundles, or `.verification` artifacts.
- Do not infer agent success from process liveness or claim that a prepared handoff has been sent.
- Preserve explicit Send, fail-closed approvals, thread ownership, and honest load/resume semantics.
- AI-assisted contributions are welcome; the contributor remains responsible for understanding, testing, and reviewing the result.

## License and conduct

By submitting a contribution, you agree to license it under the project's [MIT License](LICENSE). Do not submit code you cannot license this way. Preserve third-party license notices and describe any new dependency obligations.

Please follow our [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities using [SECURITY.md](SECURITY.md), not a public issue.
