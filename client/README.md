# Agent Code Client

Cross-platform Flutter front-end for [Agent Code](../README.md). Provides a desktop and web GUI that talks to the Rust engine through the `agent_code_client` Dart package in `../packages/agent_code_client`.

The Rust CLI (`crates/cli`) and this client are independent surfaces over the same engine — neither embeds the other. Use the CLI for terminal workflows; use this client when you want a graphical session view, point-and-click skills, or a browser-hosted agent.

## Platforms

| Platform | Status | Notes |
|----------|--------|-------|
| macOS    | ✅     | `client/macos/` |
| Linux    | ✅     | `client/linux/` |
| Web      | ✅     | `client/web/` |
| Windows  | ❌     | not yet — contributions welcome |

## Stack

- Flutter `>= 3.19.0`, Dart SDK `>= 3.0.0` (the repo pins **3.41.6** via `.fvmrc`)
- State management: `flutter_bloc` (`SessionBloc` in `lib/`)
- Markdown rendering: `flutter_markdown`
- Local package dependency: `agent_code_client` (`../packages/agent_code_client`) — handles transport to the engine and abstracts native vs. web execution

See `pubspec.yaml` for the full dependency list.

## Quickstart

Install [FVM](https://fvm.app/) so you pick up the pinned Flutter version automatically:

```bash
cd client
fvm install            # one-time, reads .fvmrc
fvm flutter pub get
fvm flutter run        # picks the connected device; pass -d macos / -d linux / -d chrome
```

If you don't use FVM, plain `flutter` works too — just make sure your local Flutter is `>= 3.19.0`.

## Tests

```bash
# Unit / widget tests
fvm flutter test

# Integration tests (in-process, runs against a real engine)
fvm flutter test integration_test/app_test.dart -d chrome
# headless:
fvm flutter test integration_test/app_test.dart -d web-server

# End-to-end browser tests (Playwright, see e2e/playwright.config.ts)
cd e2e
npm install
npx playwright test
```

## Layout

```
client/
  lib/                   Dart source (app shell, session BLoC, screens, widgets)
  test/                  Unit and widget tests
  integration_test/      In-process Flutter integration tests
  test_driver/           Drivers for `flutter drive`
  e2e/                   Playwright end-to-end browser tests (separate npm project)
  macos/  linux/  web/   Per-platform Flutter scaffolding
  pubspec.yaml           Dart/Flutter manifest
  analysis_options.yaml  Lint rules (flutter_lints)
  .fvmrc                 Pinned Flutter version (3.41.6)
```

## Contributing

The client follows the same contribution flow as the rest of the repo — see [`../CONTRIBUTING.md`](../CONTRIBUTING.md). Run `fvm flutter analyze` and `fvm flutter test` before opening a PR. CI runs the integration suite on Chrome via `chromedriver` and the Playwright e2e suite on the built web bundle.
