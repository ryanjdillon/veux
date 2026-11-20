# Contributing to veux

Thank you for helping. This page explains how a change gets into `main`.

## Workflow

All changes come from a fork. Only the maintainer pushes to this repository.

1. Open an issue first for a large change, such as a new device or a protocol change. A small fix
   needs no issue.
2. Fork the repository, and clone your fork.
3. Create a branch in your fork. Name it for the change, for example `fix-esphome-retry`.
4. Make the change. Keep one concern in each commit.
5. Run `just check`. It runs the same checks as CI: format, lint, tests, and `cargo deny`.
6. Push the branch to your fork, and open a pull request against `main`.
7. CI does not run on its own for a pull request from a fork. The maintainer must approve the run.
   Expect a short delay.
8. Answer the review comments with new commits. Do not force-push over a review in progress.
9. The maintainer merges the pull request when CI passes and the review is done.

## Set up

```bash
nix develop    # the toolchains; without Nix, see docs/development/README.md
just check
```

See [docs/development/README.md](docs/development/README.md) for the build, the toolchain, and the
layout of the code.

## What a good change has

- **Tests** for new behaviour. A driver has fixtures, not a live device. Mark a test that needs
  hardware with `#[ignore]`, and say in the test how to run it.
- **Docs in the same commit.** The drift table in [AGENTS.md](AGENTS.md) lists the docs to update
  for each kind of change.
- **A verified parameter.** Do not add a parameter to a driver schema before you verify it on a
  device or find it in the vendor command table. Cite the source in its explanation.
- **A clear message.** Write `type: Subject`, for example `fix: Retry a busy XVF3800 read`. Use
  the types `feat`, `fix`, `docs`, `refactor`, `test`, and `chore`. Write the subject as a
  command.

Write docs and comments in short sentences. Use one word for one thing, and explain why in
comments, not what.

## A new device

See [Add support for a device](docs/devices/README.md#add-support-for-a-device). Say in the pull
request which hardware and firmware versions you tested.

## Reports

- **Bugs and ideas:** open an issue. Include the device kind, the link, the firmware version, and
  the output of `veux read <device>` with any keys removed.
- **Security problems:** do not open a public issue. Use the Security tab of the repository to
  report it in private.

## License

You license your contribution under the MIT license of the project. See [LICENSE](LICENSE).
