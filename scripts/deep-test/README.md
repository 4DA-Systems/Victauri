# Deep cross-platform test kit

Run the full Victauri test battery on a rented cloud Mac (or any macOS/Linux host)
without owning Apple hardware. Designed for a Scaleway Mac mini (~€3–4 for the 24h
minimum) reached over SSH.

## One-time setup on the host
```bash
# Install Rust + tools (macOS via Homebrew shown; Linux: apt/dnf equivalents)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
# macOS:  brew install jq sqlite git
# Linux:  sudo apt-get install -y jq sqlite3 git libwebkit2gtk-4.1-dev xvfb
git clone https://github.com/4DA-Systems/victauri.git
cd victauri
```

## Run the deep battery (no GUI permissions needed)
```bash
bash scripts/deep-test/macos-deep-test.sh
```
Covers: workspace build + tests + clippy, demo-app launch, the 5-layer full-stack
proof (webview + DOM + IPC→Rust backend + registry + native), `query_db` on a
seeded SQLite, the adversarial E2E suite, and a 200-eval soak. Exits non-zero on any
hard failure; logs in `/tmp/victauri-deep-test/`.

## The two TCC-gated tools (screenshot + trusted input)
These need a one-time human grant in macOS **System Settings → Privacy & Security**:
1. Connect to the Mac via **VNC** (Scaleway Overview page shows host/port/password).
2. Grant **Screen Recording** and **Accessibility** to the Terminal (and the
   demo-app, if prompted).
3. Then:
   ```bash
   bash scripts/deep-test/macos-tcc-check.sh
   ```
   Screenshot should return a PNG. Trusted input currently reports synthetic
   (`isTrusted:false`) — the known macOS gap until `CGEvent` native input lands;
   the script flips to ✅ automatically once it does.

## After you're done
Delete the Mac mini in the Scaleway console once the 24h window is up to stop
billing. The whole battery takes minutes; the 24h is just Apple's minimum lease.
