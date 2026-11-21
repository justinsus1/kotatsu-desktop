# Disclaimer
This project is fully vibe coded

# Kotatsu Desktop

A fast, lightweight Linux desktop manga reader, built on the same sources as
[Kotatsu-Redo](https://github.com/Kotatsu-Redo/Kotatsu-Redo) for Android.

- **UI:** Rust + egui (GPU-rendered, native Wayland/X11). Uses no CPU while idle.
- **Sources:** [kotatsu-parsers-redo](https://github.com/Kotatsu-Redo/kotatsu-parsers-redo)
  (1300+ sources), compiled with GraalVM native-image into a small helper
  (`kotatsu-bridge`) that the app talks to over stdin/stdout JSON. The helper starts in ~10 ms and uses ~50 MB of RAM.

## Features

- Browse sources (filter by language, pin favourites), and search across all pinned sources at once
- Library with new-chapter checks, reading history with "continue"
- Reader: webtoon (continuous scroll) and paged (LTR / RTL) modes, preloading, keyboard controls
- Offline chapter downloads
- **Books (EPUB)**: a shelf for your `.epub` files (`~/Books`, `~/Calibre Library`, or drag files onto the window),
  ~70,000 free public-domain books from Project Gutenberg to download, your own **Calibre** library (or Kavita, Komga,
  COPS – any OPDS catalogue) to browse, search and download from, and a reader with contents, dark/sepia/light themes,
  serif/sans fonts, adjustable size, width and line spacing, and saved progress
- **Phone sync**: library and reading progress sync with Kotatsu-Redo on Android through the same sync account (Settings → Phone sync)
- Images are decoded off the UI thread, downscaled to screen size and kept within a memory budget

## Install (prebuilt, no compiling)

Linux x86_64:

```sh
curl -fsSL https://raw.githubusercontent.com/justinsus1/kotatsu-desktop/main/install.sh | bash
```

This downloads the latest release into `~/.local` (it takes seconds). Run it again to update; uninstall with
`... | bash -s -- --uninstall`. You can also grab `kotatsu-desktop-linux-x86_64.tar.gz` from the
[Releases](https://github.com/justinsus1/kotatsu-desktop/releases) page yourself.

## Build from source

Requirements: Rust (stable), `git`, `curl`. JDK 17 and GraalVM are downloaded
automatically into `.tools/` on the first build. The first build takes a while (mostly GraalVM compiling the
source helper); builds from source are tuned for your CPU.

```sh
git clone --recursive https://github.com/justinsus1/kotatsu-desktop
cd kotatsu-desktop
./build.sh            # builds everything and installs it to ~/.local
./build.sh --update   # pulls the latest sources first
./build.sh --portable --package   # what releases use: any x86-64 CPU, writes dist/*.tar.gz
```

Releases are built by GitHub Actions when a `v*` tag is pushed.

Then run `kotatsu-desktop` or launch **Kotatsu** from your app launcher.

## Reader keys

| Key | Action |
|---|---|
| ← / → / A / D | previous / next page |
| Space / PgDn, Shift+Space / PgUp | scroll a screen |
| N / P | next / previous chapter |
| M | switch reader mode |
| F / F11 | fullscreen |
| Tab / H | hide the toolbar |
| Esc | back |

## Book reader keys

| Key | Action |
|---|---|
| Space / PgDn, Shift+Space / PgUp, ↑ ↓ | scroll |
| ← / → (or P / N) | previous / next chapter |
| T | table of contents |
| + / − | font size |
| F | fullscreen |
| Esc | back |

## Phone sync

1. On the phone: Kotatsu → Settings → Services → Synchronization, and sign in (or register) on a sync server.
2. On the desktop: Settings → Phone sync, enter the same server, email and password, then press **Log in**.

It syncs at startup, every 10 minutes, when you leave the reader, and when you press **Sync now**. Only rows changed on the
desktop are sent, so the phone's newer progress is never overwritten. The default server is `https://sync.kotatsuredo.app`
(the phone app's default).

Sync test against a local mock server:

```sh
cd app
python3 tests/mock_sync_server.py 8765 &
KD_SYNC_URL=http://127.0.0.1:8765 KOTATSU_BRIDGE=$PWD/../bridge/build/native/kotatsu-bridge cargo test -- --ignored
```

## Layout

```
app/      Rust desktop app
bridge/   Kotlin helper that wraps the parsers library (built with native-image)
parsers/  kotatsu-parsers-redo (git submodule)
```

## Limitations

- Sources that need a Cloudflare check, a login, or a WebView don't work yet
- Sync covers library and history; categories are flattened into one library on desktop (desktop additions go into your first phone category)

## License

GPL-3.0, the same as the Kotatsu parsers it builds on. The developers have no affiliation
with the content providers available through the sources.
