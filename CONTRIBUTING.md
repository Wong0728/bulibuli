# Contributing

## Before opening a pull request

Keep changes focused and do not commit `data/`, `target/`, `dist/`, frontend `node_modules/`, cookies, tokens or local pairing files. Update public documentation when a command, environment variable, release asset or data path changes.

Run the checks relevant to the change:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
python build.py --check
```

Frontend changes also require:

```bash
cd static/js
npm ci --ignore-scripts
npm run build
npm run test:smoke
```

For a release-affecting change, build the local platform package with `python build.py --portable` and confirm that the archive contains the executable, `static/`, `resources/aria2c*`, `resources/ffmpeg*`, any bundled Unix `resources/lib/` files, and the matching checksums. `ffprobe` is optional because the program falls back to FFmpeg for probing. Linux CI also builds a `core` archive without media runtimes for the dependency-aware installer. Do not push a tag unless the release assets are ready.

## Commit and review expectations

Use a short imperative subject, explain behavior changes and migration impact, and include manual reproduction steps for user-visible changes. Never include secrets or copied upstream binaries without updating `NOTICE.md` and `resources/README.md`.
