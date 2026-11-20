# Foundation Studio

**Foundation Studio** is the unified Tauri v2 desktop companion for **[Foundation](https://github.com/bloooowfish/Foundation)** (an independent FFXIV housing simulator fork of [Kawari](https://codeberg.org/redstrate/Kawari)) and **[rmp-bridge](https://github.com/bloooowfish/rmp-bridge)**.

It combines FFXIV direct client launching, Foundation local server process control, in-process `rmp-bridge-relay` ReMakePlace live sync, persistent local preset folder monitoring, and the Housing Studio interface into a single native Windows application.

## Key Features

- **FFXIV Direct Launcher**: Queries `/.well-known/xiv`, authenticates against Foundation's login endpoint, reads `ffxivgame.ver`, builds `DEV.LobbyHost01..12` launch arguments, detects already-running `ffxiv_dx11.exe` instances, and encrypts saved passwords with Windows DPAPI (`CurrentUser`).
- **Foundation Server Control**: Automatically detects an already-running external server on `:21064`, or starts/stops `kawari-run.exe` inside a Windows Job Object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) with Oodle/port preflight checks and live console log streaming so child server processes never orphan.
- **In-Process ReMakePlace Live Sync**: Provisions the zero-touch [`rmp-bridge`](https://github.com/bloooowfish/rmp-bridge) shadow runtime (`prepare-shadow.ps1`), launches shadow ReMakePlace, and runs the `rmp-bridge-relay` coordinator in-process with hook/actor/sync telemetry.
- **Unified Housing Studio UI**: Rust-proxied REST & SSE connection (`:21064/api/v1/housing/*`) avoiding WebView2 CORS/Private Network Access friction, with multi-estate management, furniture transform nudging, environment controls, snapshots/undo, and activity logs.
- **Persistent Preset Folder Watcher**: Watches configured folders (including drag-and-dropped folders/files) with `notify`, decodes UTF-8/UTF-16 BOM ReMakePlace JSON presets, supports optional auto-apply on save, exports directly to disk, and opens files in Windows Explorer.
- **5-Language Localization (`ko` / `en` / `ja` / `de` / `fr`)**: Externalized UI strings and client housing terminology under `frontend/locales/*.json`, with automatic Windows OS locale detection (`GetUserDefaultLocaleName`) and live switching across both the dashboard and system tray menu without restarting.
- **Desktop Integration**: System tray icon, configurable global show/hide hotkey (default `Ctrl+Shift+H`), Always-on-Top pin toggle, single-instance guard, and safe minimize-to-tray when background sync/server processes are active.

## Prerequisites & Building

No Node.js, npm, Vite, or `cargo-tauri` CLI is required—the frontend uses unbundled vanilla HTML/CSS/JS embedded via Tauri's `custom-protocol`.

- Rust toolchain (`rustc` / `cargo`, edition 2024) + Windows MSVC build tools
- Microsoft Edge WebView2 Runtime (preinstalled on Windows 10/11)

`Cargo.toml` references `rmp-bridge-relay` at `../rmp-bridge/relay`, so clone **[Foundation-Studio](https://github.com/bloooowfish/Foundation-Studio)** and **[rmp-bridge](https://github.com/bloooowfish/rmp-bridge)** side-by-side in the same parent folder:

```powershell
git clone https://github.com/bloooowfish/Foundation-Studio.git
git clone https://github.com/bloooowfish/rmp-bridge.git
cd Foundation-Studio

# Run unit tests & lint
cargo test
cargo clippy --all-targets -- -Dwarnings

# Build single release binary
cargo build --release
```

Output executable: `target\release\foundation-studio.exe`.

## Settings & Automatic Migration

Settings are stored at `%AppData%\FoundationStudio\settings.json` with passwords protected via Windows DPAPI.

On first launch (when `%AppData%\FoundationStudio\settings.json` does not yet exist), Foundation Studio automatically imports existing settings read-only from `%AppData%\KawariStudio\settings.json` or `%AppData%\KawariLauncher\settings.json` without modifying legacy config files.

## Credits & Acknowledgements

- **[Kawari](https://codeberg.org/redstrate/Kawari)** by **redstrate** and Kawari contributors — for the upstream server foundation, autoconfig endpoint, and direct-launch protocol. Foundation Studio is an independent companion project for the Foundation fork and is not affiliated with or supported by upstream Kawari.
- **MakePlace & [ReMakePlace](https://github.com/RemakePlace/app)** — originally created by **jawslouis** and maintained by the ReMakePlace community.
- **[RE-UE4SS](https://github.com/UE4SS-RE/RE-UE4SS)** (MIT License) — powering Lua scripting inside the shadow ReMakePlace runtime (downloaded automatically by [`rmp-bridge`](https://github.com/bloooowfish/rmp-bridge) from official releases on first run).
- **[Tauri](https://tauri.app)** — native desktop runtime.

## License & Legal Notices

- **License**: Foundation Studio is licensed under the **[GNU General Public License v3.0 or later (GPL-3.0-or-later)](LICENSE)**.
- **Companion & Third-Party Licenses**:
  - **[`rmp-bridge`](https://github.com/bloooowfish/rmp-bridge)** (`rmp-bridge-relay`): Licensed under **GPL-3.0-or-later** ([`rmp-bridge/LICENSE`](https://github.com/bloooowfish/rmp-bridge/blob/main/LICENSE)).
  - **[`Foundation`](https://github.com/bloooowfish/Foundation)**: Operated and launched only as an external child process/HTTP+SSE service and remains separately licensed under the **GNU Affero General Public License v3.0 (AGPL-3.0)** ([`Foundation/LICENSE`](https://github.com/bloooowfish/Foundation/blob/main/LICENSE)).
  - **RE-UE4SS**: Automatically downloaded from official GitHub Releases by `rmp-bridge` (`prepare-shadow.ps1`) with SHA-256 verification and loaded inside the shadow ReMakePlace runtime; licensed under the **[MIT License](https://github.com/UE4SS-RE/RE-UE4SS/blob/main/LICENSE)**.
  - **Rust / Tauri Dependencies**: Direct and transitive crate dependencies retain their respective open-source licenses (predominantly MIT/Apache-2.0/BSD/ISC, plus unmodified MPL-2.0 crates).
- **Trademarks, Game Terminology & Non-Affiliation**:
  - *FINAL FANTASY* and *FINAL FANTASY XIV* are registered trademarks of Square Enix Co., Ltd. Foundation Studio is an independent, non-commercial community companion tool and is **not affiliated with, endorsed by, or supported by** Square Enix, upstream Kawari, MakePlace / ReMakePlace, or RE-UE4SS.
  - Localized district, zone, style, and fixture terms in `frontend/locales/*.json` are used solely for UI identification; no copyrighted game assets, retail executables, or proprietary third-party libraries (such as `oodle-network-shared.dll`) are distributed in this repository.
- **Development Transparency**: Portions of this companion application and its tooling were created with AI-assisted software engineering.
