# CS Captain's Log

Electron-based activity tracker and captain's log for Star Citizen organisations.

Built as a local-first desktop tool for logging player interactions, kills, deaths, trading, mining, bounties, captures, contacts and operational notes.

## Features

- Offline desktop operation
- Kill, death, capture and bounty logging
- Commodity, player-trade, mining and harvesting records
- Player/contact dossiers
- Faction theme presets
- Local settings and organisation customisation
- Windows installer and portable build support

## Planned Features

- Game log monitoring and event confirmation prompts where supported by available Star Citizen logging
- Import/export support for organisation records and log
- Optional self-hosted database backend for organisation-wide records
- Discord bot integration for querying player dossiers, activity logs, and stats within org Discord servers
- Role-based access controls for shared org data
- Sync support between local app data and the hosted backend

## Built With

- Electron
- Node.js
- Vanilla JavaScript
- HTML/CSS
- Electron Builder

## Running in development

```bash
npm install
npm start
```

## Building a distributable

```bash
npm install
npm run build-win
```

This produces Windows builds in the `dist/` folder, including:

- `CS Captain's Log Setup 1.0.0.exe` — installer version
- `CS Captain's Log 1.0.0.exe` — portable version

Users do **not** need Node.js installed to run the built app.

## User data

All entered data is saved locally on the user's own machine using Electron local storage.

Existing user data is intentionally preserved when reinstalling or updating the app.

To test a clean first run, close the app and delete the relevant folder in `%APPDATA%`.

Depending on the version/build history, older test installs may have used `%APPDATA%\cs-tracker\`. New builds are named **CS Captain's Log**.

## Roadmap

- Self-hosted database backend
- Discord bot integration
- API-driven stat queries
- Multi-user sync
- Organisation dashboards

## Distribution

Source code should live in this repository.

Compiled `.exe` files should be attached to GitHub Releases rather than committed directly to the repository.
