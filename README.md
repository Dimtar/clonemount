# clonemount

Mount [rclone](https://rclone.org) remotes as drives on Windows, macOS and Linux. Set them up once in a small window; after that clonemount starts at login, mounts everything in the background and sits in the tray.

## Using it

1. **Remotes**: add a remote (Google Drive, OneDrive, S3, SFTP, …), or import the ones you already have in rclone's config.
2. **Drives**: pick a remote and, optionally, a folder in it. Leave *Mount at* empty to get a free drive letter on Windows, or `~/clonemount/<name>` on macOS and Linux.
3. Close the window. clonemount keeps running in the tray and remounts everything when you next log in.

If a mount fails, for example because the network isn't up yet right after boot, clonemount keeps retrying with a growing delay, up to once a minute.

### Moving to another computer

Your drives and clonemount's own copy of `rclone.conf` are stored together in one data folder. **Settings → Export** writes both into one file, and **Import** on the other computer restores them. Automatic mount locations work on any OS, so one export works on Windows, macOS and Linux.

The export contains your remote credentials. Treat it like a password.

**Portable mode:** create a folder called `clonemount-data` next to the clonemount executable, and clonemount keeps everything there instead of in your user profile. Handy on a USB stick.

## What each OS needs

clonemount comes with its own copy of rclone. Beyond that:

| OS | Needs |
|---|---|
| Windows 10/11 | [WinFsp](https://winfsp.dev/rel/). clonemount tells you if it's missing. |
| macOS | Nothing. clonemount uses rclone's NFS mount, which needs no kernel extension. If macFUSE is installed it uses that instead. The first time you open a mount, macOS may ask whether to allow access to network volumes. |
| Linux | FUSE (`fuse3`), which most distros already include. |

## Developing

Needs Node and [Rust](https://rustup.rs).

```bash
npm install
npm run dev
```

The first run downloads rclone for your platform into `src-tauri/binaries/` (checked against rclone's published SHA256 sums). To change the bundled version, edit `RCLONE_VERSION` in `scripts/fetch-rclone.mjs`.

`npm run build` makes installers for the current OS. Pushing a `v*` tag builds installers for all platforms into a draft GitHub release (see `.github/workflows/release.yml`).

### Layout

- `src/`: the window UI (plain HTML, CSS and JS, no build step)
- `src-tauri/src/config.rs`: settings, rclone.conf, export and import
- `src-tauri/src/mounts.rs`: starts, watches, retries and stops the `rclone mount` processes
- `src-tauri/src/rclone.rs`: finds and runs the rclone binary
- `src-tauri/src/lib.rs`: Tauri commands, tray and autostart
