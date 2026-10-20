# GridCraft on Android (experimental)

GridCraft's desktop interface running on Android tablets, meant for use with a keyboard, an
S Pen and, if you like, a mouse. Built from the `android` branch by
`.github/workflows/android.yml` for arm64 devices running Android 11 or later. The approach is
the one the PdfCraft and WordCraft Android ports use (a NativeActivity APK built without Gradle).

## Install

1. On the tablet, open the repository's **Releases** page and download `gridcraft.apk` from the
   `android-latest` prerelease.
2. Open the downloaded file. Android asks to allow installs from your browser or Files app the
   first time; allow it, then install. If Play Protect warns about an unknown app, choose
   *More details ▸ Install anyway*.
3. Give GridCraft file access. It is a special access, not one of the app's regular permissions:
   the first time you open or save a file, the file browser shows **Allow access…**, which opens
   the switch in Settings. Or turn it on yourself: **Settings ▸ Apps ▸ ⋮ ▸ Special access ▸ All
   files access ▸ GridCraft**. Without it GridCraft can only open and save files in its own app
   folder.

Later builds install over the earlier one as long as they are signed with the same key.

## Updates

GridCraft updates itself from the GitHub repository it was built from. At start-up, and at most
once an hour after that (when it's open or comes back to the front), it asks GitHub for the
`android-latest` release. When that is a newer build than the one installed, a card at the bottom
right offers it:

1. **Update** downloads the APK inside the app, with a progress bar.
2. The first time, Android asks you to allow GridCraft to install apps: turn on **Allow from this
   source** in the settings that open, come back and tap **Install**.
3. Android shows its own install window: tap **Update**. GridCraft closes and comes back on the
   new build, so save your workbooks first.

**Later** hides the card for that build until GridCraft next starts. Being offline is fine: a
check that fails says nothing and tries again later. Android only installs an update signed with
the same key as the installed app, so a download that isn't one of this repository's builds is
refused.

## Open workbooks from other apps

GridCraft appears in **Open with** for Excel workbooks (.xlsx, .xlsm, .xltx, .xlsb, .xls),
OpenDocument spreadsheets (.ods) and CSV/TSV files, from Files, My Files, Gmail, Drive and Chrome
downloads. Choose GridCraft and **Always** to make it the default.

A workbook from a file in your storage opens as that file, and Save writes it in place (with All
files access). A workbook that only exists in another app (an email attachment, a Drive file) is
first saved as a copy in **Documents/GridCraft** (or the app folder, without All files access),
and that copy is what you edit and save.

## How it differs from the desktop app

- **Workbook tabs:** each open workbook has a tab along the top. File ▸ New and the **+** at the
  end of the tabs add one; File ▸ Open and workbooks sent from other apps open in a tab of their
  own (Open replaces an untouched blank Book1). A tab's **×**, File ▸ Close or Ctrl+W closes that
  workbook (asking to save changes first). Ctrl+Tab and Ctrl+Shift+Tab move between tabs, and a
  dot after a tab's name marks unsaved changes.
- **Files:** Open, Save As, the exports (PDF, CSV, web page), Save a Copy and Insert Picture use
  GridCraft's own file browser, with shortcuts to Internal storage, Documents, Download, SD cards
  and USB drives, and the app folder. A tap opens a folder; tap a file, then the button.
- **Touch:** a finger tap clicks, a finger drag scrolls the sheet, and a finger held still for
  half a second right-clicks (context menus). Two fingers pan, and pinch to zoom. Fingers don't
  select ranges: use the S Pen, a mouse or Shift+arrows.
- **S Pen:** works like a mouse with the button held: tap to pick a cell, drag to select a range,
  click buttons and menus. While the pen is on the screen, fingers and a resting palm are ignored.
- **Mouse:** a paired Bluetooth or USB mouse works as on a desktop (hover, right-click, wheel,
  Ctrl+wheel zoom), and so does a touchpad on a keyboard cover.
- **Keyboard:** with a keyboard cover or Bluetooth keyboard attached, the on-screen keyboard stays
  away; without one, it comes up when you type into a cell or a field. Desktop shortcuts work.
- **Clipboard:** cells you copy go to other apps as a table (HTML, with tab-separated text for
  apps that want plain text), and Paste takes text and tables copied in other apps.
- **Links** in cells open in the browser or mail app.
- **Not on Android:** printing to a printer (export a PDF instead) and the `--control`
  automation channel.
- Settings (`ui.json`, `prefs.json`) and logs live in the app's private folder; uninstalling
  removes them. Fonts: Android's own (Roboto, Noto) plus egui's built-in ones.

## Build it yourself

Needs the Android SDK and NDK, `rustup target add aarch64-linux-android` and
`cargo install cargo-ndk`:

```sh
ANDROID_HOME=~/Android/Sdk ANDROID_NDK_HOME=~/Android/Sdk/ndk/<version> packaging/android/build-apk.sh
adb install -r target/android-apk/gridcraft.apk
adb logcat -s GridCraft   # the app's log
```

The build log of every CI run is also pushed to the `ci-logs` branch (`build.log`, and the
compiler errors alone in `errors.txt`).

## Signing

The APK is signed with `packaging/android/debug.keystore` (password `android`), which is public,
like Android's own debug key. To use a private key instead, add the repository secrets
`ANDROID_KEYSTORE_B64` (the keystore, base64), `ANDROID_KEYSTORE_PASSWORD` and
`ANDROID_KEY_ALIAS`. Android won't install an update signed with a different key over an
installed app: uninstall the debug-signed one first.

## How it's put together

| Part | What it does |
|---|---|
| `apps/gridcraft-android` | The `cdylib` Android loads: `android_main` and the host services (`lib.rs`), `tablet.rs` (insets, workbook tabs, keyboard, clipboard sync), `touch.rs` (pen, finger and palm routing), `files.rs` (the file browser), `file_access.rs`, `incoming.rs`, `clipboard.rs`, `updater.rs` (update check and card) |
| `crates/ui-egui` | Unchanged UI, plus one host option any platform can use: `Services::ask_file`, answered later with `SheetApp::finish_file_request`; and Android's font folders |
| `vendor/winit` | winit 0.30.13 with PdfCraft's Android patches (tool type per touch, mouse and touchpad as a mouse); see `vendor/README.md` |
| `packaging/android` | Manifest, the Java activity (intents, the updater's HTTP and PackageInstaller install, system bar colours), signing key, `build-apk.sh` |
