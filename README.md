# Termite

A modern, open-source SSH client for Linux, macOS, and Windows — built entirely in Rust.

> "It should disappear and let you work."

## Philosophy

Most SSH clients either look outdated, require cloud accounts, lock features behind subscriptions, or feel bloated. Termite is different:

- **No accounts.** Everything stays on your machine.
- **No telemetry.** We do not phone home.
- **No subscriptions.** All features, forever, free.
- **No AI.** Just a fast, reliable SSH client.
- **Local-first.** Your keys, your config, your machine.

## Status

> Early development. Not yet usable.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design and [the roadmap](#roadmap) below.

## Roadmap

| Milestone | Description | Status |
|-----------|-------------|--------|
| M0 | Project skeleton, CI, window opens | In progress |
| M1 | Local terminal emulator | Pending |
| M2 | SSH core (password auth) | Pending |
| M3 | Key authentication & credential storage | Pending |
| M4 | Host management UI | Pending |
| M5 | Tabs & multi-session | Pending |
| M6 | Advanced terminal features | Pending |
| M7 | Port forwarding, SFTP | Pending |
| M8 | Command palette, UX polish | Pending |
| M9 | Quality, security review, public release | Pending |

## Building

Requirements: Rust 1.78+ (stable)

```sh
git clone https://github.com/YOUR_USERNAME/termite
cd termite
cargo build
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

See [SECURITY.md](SECURITY.md) for the responsible disclosure policy.

## License

MIT — see [LICENSE](LICENSE).
