# Hooviestar branding source

`hooviestar-icon-source.png` is the high-resolution source for the Hooviestar application mark. It was generated with OpenAI Imagegen on 2026-08-31, then refined to remove a simulated transparency grid and simplify the silhouette for small desktop sizes.

The mark combines two overlapping frames for Studio and Program with an owl eye and four-point star. Its indigo-violet, electric-blue, charcoal, and white palette matches the Studio UI.

Generate platform derivatives with the pinned Tauri CLI:

```sh
npm run tauri -- icon assets/branding/hooviestar-icon-source.png --output /tmp/hooviestar-icons
```

Hooviestar currently ships the generated `src-tauri/icons/icon.png`, multi-resolution `src-tauri/icons/icon.ico`, and compact `src/assets/hooviestar-icon-64.png` Studio mark. The release check pins source size, PNG sizes, and all ICO frames.
