# Cleanup notes

This pass keeps the existing app functionality and aesthetics intact while cleaning project structure and naming.

## Changed

- Standardised project name to `CS Captain's Log`.
- Updated package metadata and lockfile root name.
- Set Electron app name explicitly in `main.js`.
- Updated README and release README wording.
- Added/updated `.gitignore` to keep generated builds, dependencies and logs out of Git.

## Not changed

- UI layout
- Styling/aesthetic
- App logic
- Local data model
- Electron preload API

## Before publishing

1. Run `npm install`.
2. Run `npm start` and test locally.
3. Delete any old test data from `%APPDATA%` if you want to test a clean first run.
4. Run `npm run build-win`.
5. Upload source to GitHub.
6. Upload `.exe` files to GitHub Releases, not directly into the repo.
