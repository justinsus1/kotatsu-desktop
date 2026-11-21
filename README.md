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
- Images are decoded off the UI thread, downscaled to screen size and kept within a memory budget

## Build

Requirements: Rust (stable), `git`, `curl`. JDK 17 and GraalVM are downloaded
automatically into `.tools/` on the first build.

```sh
git clone --recursive https://github.com/justinsus1/kotatsu-desktop
cd kotatsu-desktop
./build.sh            # builds everything and installs it to ~/.local
./build.sh --update   # pulls the latest sources first
```

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

## Layout

```
app/      Rust desktop app
bridge/   Kotlin helper that wraps the parsers library (built with native-image)
parsers/  kotatsu-parsers-redo (git submodule)
```

## Limitations

- Sources that need a Cloudflare check, a login, or a WebView don't work yet
- No sync with the phone app yet

## License

GPL-3.0, the same as the Kotatsu parsers it builds on. The developers have no affiliation
with the content providers available through the sources.
