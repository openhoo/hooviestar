# Windows integration review

Reviewed 2026-08-31 against Tauri 2.11.5, Tauri CLI 2.11.4, the checked-in Tauri v2 schema, and the official Tauri v2 documentation.

## Applied baseline

Hooviestar now publishes explicit bundle metadata instead of relying on identifier-derived defaults:

- publisher `OpenHoo`, MIT license, project homepage, copyright, and `Video` category;
- short and long package descriptions for NSIS and Linux packages;
- a generated 512 px PNG plus six-frame Windows ICO (16, 24, 32, 48, 64, and 256 px);
- a dark native window theme and matching startup background, avoiding a light WebView flash;
- work-area overflow prevention for restored or resized Studio windows;
- NSIS downgrade blocking, English and German installer resources, and current-user installation;
- an embedded WebView2 bootstrapper. This adds roughly 1.8 MB and still needs a network connection when WebView2 is absent. The roughly 127 MB offline installer is not justified for the current audience;
- `tauri-plugin-single-instance` 2.4.4 as the first plugin: another launch restores, shows, and focuses the existing Studio instead of starting a second renderer, Program window, or hotkey set. A bounded one-second retry covers launches racing the first setup. Arguments remain intentionally ignored until a validated project-open contract exists;
- `tauri-plugin-window-state` 2.4.1 restricted by label filter to `studio`. It stores only position, size, and maximized state; visibility, fullscreen, decorations, Program, and future utility windows stay outside persistence;
- Tauri core taskbar progress for update checking, measured downloads, installation, and GPU-device recovery. Independent state tracking keeps a device failure red even if an unrelated updater operation succeeds. Windows additionally gets a generated red error overlay; success clears only the matching subsystem state.

The signed release pipeline, updater signatures, Authenticode, timestamps, immutable releases, SBOM, provenance, and updater manifest remain separate security controls. Metadata does not replace any of them.

## Implemented high-value native features

1. **Single instance** — plugin registered first, with show, unminimize, focus, and bounded startup-race recovery. No unvalidated second-instance arguments reach engine commands.
2. **Taskbar progress and overlay state** — updater download percentages and indeterminate check/install/recovery phases use Tauri core. Updater or GPU-recovery failures use Windows error state and overlay. The in-app status bar remains authoritative.
3. **Window-state persistence** — only Studio position, non-maximized size, and maximized state persist. `preventOverflow` and the plugin's available-monitor intersection check protect monitor removal; malformed state falls back to defaults.

Local Rust and configuration tests cover state precedence, download percentages, plugin ordering, restore flags, and activation calls. Actual foreground focus, taskbar rendering, DPI changes, and monitor-removal behavior still require the interactive Windows qualification before release claims.

### Useful after product behavior exists

- **System tray:** Tauri core provides tray icons, native menus, click events, and tooltip state. Add only with a clear minimize-to-tray contract, visible “Studio öffnen” and “Beenden” actions, and no hidden background capture surprise.
- **Notifications:** `tauri-plugin-notification` fits completed updates, device-recovery failure, or unavailable sources while Studio is unfocused. Request permission in context; do not notify for high-frequency mixer or capture events.
- **Autostart:** `tauri-plugin-autostart` must be an explicit setting, default off. Starting capture software and registering global hotkeys at login without consent would be hostile.
- **File associations and deep links:** bundle `fileAssociations` plus `tauri-plugin-deep-link` become useful only after Hooviestar has an intentional project-file extension or a documented `hooviestar://` command contract. Validate all payloads as untrusted input and route second-instance opens through the single-instance handler.
- **Jump lists:** useful for opening Studio or selecting a recent project, but not a first-class Tauri core feature. Implement through the Windows crate only after project-open semantics exist.

### Defer

- **Mica, Tabbed, Blur, and Acrylic:** Tauri exposes these through `windowEffects`. Mica variants require Windows 11; Blur/Acrylic have documented resize performance caveats, and effects require transparency. Hooviestar embeds native preview surfaces and has strict Discord-capture behavior, so visual value does not justify the compositing and qualification risk yet.
- **`contentProtected`:** could reduce accidental Studio capture, but may interact with embedded preview and Windows Graphics Capture. Enable only after real Windows/Discord qualification proves Studio, Program, preview, screenshots, and recovery paths separately.
- **`windowClassname`:** can provide a stable Windows automation/capture identity, but becomes a compatibility contract. Current title and native HWND ownership are sufficient.
- **Browser extensions and custom WebView arguments:** unnecessary capability and support surface. Keep disabled/default.

## Security and qualification rules

- Every new plugin needs minimal Tauri capabilities and an explicit Rust initialization point.
- Deep-link, file-open, notification, tray, and second-instance payloads cross trust boundaries; validate before dispatching engine commands.
- Features touching process lifetime, native windows, capture exclusion, taskbar identity, transparency, or startup must pass the interactive Windows/Discord qualification, not only hosted CI.
- Preserve the dedicated mapped Program window. Do not replace it with a hidden WebView window; Discord needs a capturable native application surface.
- Do not enable per-machine NSIS installation unless a system-wide service or shared resource actually requires administrator privileges.

## Primary references

- [Tauri configuration files](https://v2.tauri.app/develop/configuration-files/)
- [Windows installers and WebView2 modes](https://v2.tauri.app/distribute/windows-installer/)
- [Windows code signing](https://v2.tauri.app/distribute/sign/windows/)
- [Single-instance plugin](https://v2.tauri.app/plugin/single-instance/)
- [Window-state plugin](https://v2.tauri.app/plugin/window-state/)
- [Tauri Rust `Window` API](https://docs.rs/tauri/latest/tauri/window/struct.Window.html)
- [Autostart plugin](https://v2.tauri.app/plugin/autostart/)
- [Deep-link plugin](https://v2.tauri.app/plugin/deep-linking/)
- [Notification plugin](https://v2.tauri.app/plugin/notification/)
