# Refactor Step 1 Notes

This version performs the lowest-risk separation of the original single-file app:

- `src/index.html` keeps the existing markup and inline event attributes.
- `src/styles.css` contains the extracted CSS from the original `<style>` block.
- `src/app.js` contains the extracted JavaScript from the original `<script>` block.

No behavioural logic has intentionally been changed. This is not yet a full ES module refactor; it is a safe first step so the app can still use the existing global functions referenced by `onclick`, `oninput`, and other inline handlers.

Recommended next refactor steps:

1. Move constants/default data into `src/js/data.js`.
2. Move storage helpers into `src/js/storage.js`.
3. Move render functions by feature: kills, captures, trades, players, settings.
4. Replace inline HTML event attributes with `addEventListener` bindings.
5. Only then convert to true `type="module"` JavaScript.

Do not commit build output, `.exe` files, `.blockmap` files, `builder-debug.yml`, or `builder-effective-config.yaml` to GitHub.
